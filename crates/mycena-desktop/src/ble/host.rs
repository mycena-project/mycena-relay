// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! BLE central + peripheral host (`blew`) — F1 handoff, F2 ingest/relay.

use crate::ble::bridge;
use crate::ble::gatt::{INVITE_CHAR_UUID, MESH_DATA_CHAR_UUID, SERVICE_UUID};
use crate::ble::link_registry::LinkRegistry;
use crate::discovery::MeshDiscoveryState;
use blew::central::{CentralEvent, ScanFilter, ScanMode, WriteType};
use blew::gatt::props::{AttributePermissions, CharacteristicProperties};
use blew::gatt::service::{GattCharacteristic, GattService};
use blew::peripheral::{AdvertisingConfig, Peripheral, PeripheralRequest};
use blew::types::{BleDevice, DeviceId};
use blew::Central;
use mycena_app::node::AppNode;
use mycena_l0_facade::ble_mesh::ChunkAssembler;
use mycena_l1_facade::NodeHandle;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};
use tokio_stream::StreamExt;
use tracing::{debug, info, warn};

const READY_TIMEOUT: Duration = Duration::from_secs(15);
const RELAY_INTERVAL: Duration = Duration::from_secs(2);
const INVITE_REFRESH: Duration = Duration::from_secs(30);
const HANDOFF_CONNECT: Duration = Duration::from_secs(12);
const CHUNK_WRITE_DELAY: Duration = Duration::from_millis(25);
const SCAN_HEARTBEAT: Duration = Duration::from_secs(45);

fn is_mycena_advertisement(device: &BleDevice) -> bool {
    device.services.contains(&SERVICE_UUID) || device.name.as_deref() == Some("Mycena")
}

pub struct BleHostContext {
    pub handle: NodeHandle,
    pub app_node: Arc<AppNode>,
    pub local_peer_id: String,
    pub links: Arc<LinkRegistry>,
    pub discovery: Arc<MeshDiscoveryState>,
}

pub async fn run(ctx: BleHostContext, mut shutdown: watch::Receiver<bool>) {
    if run_inner(ctx, &mut shutdown).await.is_err() {
        warn!("desktop BLE host stopped with error");
    }
}

async fn run_inner(
    ctx: BleHostContext,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let central: Central = Central::new().await?;
    let peripheral: Peripheral = Peripheral::new().await?;

    central.wait_ready(READY_TIMEOUT).await?;
    peripheral.wait_ready(READY_TIMEOUT).await?;

    if !central.is_powered().await? {
        anyhow::bail!("Bluetooth adapter is off — enable it in System Settings");
    }

    let invite = Arc::new(Mutex::new(
        bridge::invite_uri(&ctx.handle, &ctx.local_peer_id)
            .await
            .unwrap_or_default(),
    ));

    peripheral
        .add_service(&GattService {
            uuid: SERVICE_UUID,
            primary: true,
            characteristics: vec![
                GattCharacteristic {
                    uuid: INVITE_CHAR_UUID,
                    properties: CharacteristicProperties::READ,
                    permissions: AttributePermissions::READ,
                    value: vec![],
                    descriptors: vec![],
                },
                GattCharacteristic {
                    uuid: MESH_DATA_CHAR_UUID,
                    properties: CharacteristicProperties::WRITE
                        | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
                    permissions: AttributePermissions::WRITE,
                    value: vec![],
                    descriptors: vec![],
                },
            ],
        })
        .await?;

    let mut requests = peripheral
        .take_requests()
        .ok_or_else(|| anyhow::anyhow!("peripheral request stream already taken"))?;

    // Scan without OS service filter — some phones put the 128-bit UUID only in
    // scan response; we match in software (service UUID or local name "Mycena").
    central
        .start_scan(ScanFilter {
            services: vec![],
            mode: ScanMode::LowLatency,
        })
        .await?;
    ctx.discovery.on_ble_bootstrap_started().await;
    info!("desktop BLE: scanning for Mycena peers (broad scan)");

    peripheral
        .start_advertising(&AdvertisingConfig {
            local_name: "Mycena".into(),
            service_uuids: vec![SERVICE_UUID],
        })
        .await?;
    info!("desktop BLE: advertising Mycena GATT service");

    let central = Arc::new(central);
    let mut events = central.events();
    let assemblers: Arc<Mutex<HashMap<String, ChunkAssembler>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let handed_off: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let in_flight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let connected: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    let mut relay_tick = tokio::time::interval(RELAY_INTERVAL);
    relay_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut invite_tick = tokio::time::interval(INVITE_REFRESH);
    invite_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scan_heartbeat = tokio::time::interval(SCAN_HEARTBEAT);
    scan_heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scan_seen: u64 = 0;
    let mut mycena_seen: u64 = 0;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_ok() && *shutdown.borrow() {
                    break;
                }
            }
            _ = invite_tick.tick() => {
                let next = bridge::invite_uri(&ctx.handle, &ctx.local_peer_id).await.unwrap_or_default();
                if !next.is_empty() {
                    *invite.lock().await = next;
                }
            }
            _ = relay_tick.tick() => {
                flush_relay(&ctx, central.as_ref(), &connected).await;
            }
            _ = scan_heartbeat.tick() => {
                debug!(
                    "desktop BLE: scan active — {mycena_seen} Mycena / {scan_seen} total advertisements seen"
                );
            }
            req = requests.next() => {
                match req {
                    Some(PeripheralRequest::Read { char_uuid, offset, responder, client_id, .. }) => {
                        if char_uuid == INVITE_CHAR_UUID {
                            let full = invite.lock().await.clone().into_bytes();
                            let slice = if (offset as usize) >= full.len() {
                                Vec::new()
                            } else {
                                full[(offset as usize)..].to_vec()
                            };
                            debug!("BLE invite read from {client_id} offset={offset}");
                            responder.respond(slice);
                        } else {
                            responder.error();
                        }
                    }
                    Some(PeripheralRequest::Write { char_uuid, value, responder, client_id, .. }) => {
                        if char_uuid == MESH_DATA_CHAR_UUID {
                            if let Some(r) = responder {
                                r.success();
                            }
                            ingest_peripheral_write(
                                &ctx,
                                central.clone(),
                                client_id.clone(),
                                &value,
                                &assemblers,
                            ).await;
                        } else if let Some(r) = responder {
                            r.error();
                        }
                    }
                    None => break,
                }
            }
            ev = events.next() => {
                match ev {
                    Some(CentralEvent::DeviceDiscovered(device)) => {
                        scan_seen = scan_seen.saturating_add(1);
                        if !is_mycena_advertisement(&device) {
                            continue;
                        }
                        mycena_seen = mycena_seen.saturating_add(1);
                        let device_id = device.id.as_str().to_string();
                        ctx.discovery.on_nearby_sighting(&device_id).await;
                        debug!(
                            "BLE peer discovered id={device_id} rssi={:?} name={:?}",
                            device.rssi, device.name
                        );
                        if !in_flight.lock().await.insert(device_id.clone()) {
                            continue;
                        }
                        let ctx = ctx.clone();
                        let handed_off = Arc::clone(&handed_off);
                        let in_flight = Arc::clone(&in_flight);
                        let connected = Arc::clone(&connected);
                        let central = Arc::clone(&central);
                        tokio::spawn(async move {
                            try_handoff(
                                ctx,
                                central,
                                DeviceId::from(device_id.clone()),
                                device.rssi,
                                handed_off,
                                in_flight,
                                connected,
                            )
                            .await;
                        });
                    }
                    Some(CentralEvent::DeviceConnected { device_id }) => {
                        connected.lock().await.insert(device_id.to_string());
                    }
                    Some(CentralEvent::DeviceDisconnected { device_id, .. }) => {
                        connected.lock().await.remove(device_id.as_str());
                    }
                    Some(_) | None => {}
                }
            }
        }
    }

    let _ = central.stop_scan().await;
    let _ = peripheral.stop_advertising().await;
    ctx.discovery.on_ble_bootstrap_stopped().await;
    info!("desktop BLE host shut down");
    Ok(())
}

impl BleHostContext {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            app_node: Arc::clone(&self.app_node),
            local_peer_id: self.local_peer_id.clone(),
            links: Arc::clone(&self.links),
            discovery: Arc::clone(&self.discovery),
        }
    }
}

async fn ingest_peripheral_write(
    ctx: &BleHostContext,
    central: Arc<Central>,
    client_id: DeviceId,
    chunk: &[u8],
    assemblers: &Arc<Mutex<HashMap<String, ChunkAssembler>>>,
) {
    let from_peer = ensure_peer_for_client(ctx, central, &client_id).await;
    let Some(peer) = from_peer else {
        warn!("mesh chunk from unknown central {client_id}");
        return;
    };
    let client_key = client_id.as_str().to_string();
    let mut map = assemblers.lock().await;
    let assembler = map.entry(client_key).or_default();
    let frame = match assembler.push_chunk(chunk) {
        Ok(Some(f)) => f,
        Ok(None) => return,
        Err(e) => {
            warn!("BLE chunk assemble from {peer}: {e}");
            assembler.reset();
            return;
        }
    };
    drop(map);
    if let Err(e) = bridge::ingest_ble_frame(&ctx.handle, &peer, frame).await {
        warn!("ingest BLE frame from {peer}: {e}");
    } else {
        debug!("ingested BLE frame from {peer}");
    }
}

/// When a phone connects as GATT central to our peripheral, read its invite (Android parity).
async fn ensure_peer_for_client(
    ctx: &BleHostContext,
    central: Arc<Central>,
    client_id: &DeviceId,
) -> Option<String> {
    if let Some(peer) = ctx.links.peer_for_device(client_id.as_str()) {
        return Some(peer);
    }
    if tokio::time::timeout(HANDOFF_CONNECT, central.connect(client_id))
        .await
        .ok()?
        .is_err()
    {
        return None;
    }
    if tokio::time::timeout(HANDOFF_CONNECT, central.discover_services(client_id))
        .await
        .ok()?
        .is_err()
    {
        return None;
    }
    let bytes = tokio::time::timeout(
        HANDOFF_CONNECT,
        central.read_characteristic(client_id, INVITE_CHAR_UUID),
    )
    .await
    .ok()?
    .ok()?;
    let invite = String::from_utf8(bytes).ok()?;
    let invite = invite.trim();
    if !invite.starts_with("mycena://") {
        return None;
    }
    let peer = bridge::peer_id_from_invite(invite)?;
    if peer.is_empty() {
        return None;
    }
    ctx.links.register(&peer, client_id.as_str());
    bridge::register_ble_link(&ctx.handle, &peer, client_id.as_str()).await;
    Some(peer)
}

async fn try_handoff(
    ctx: BleHostContext,
    central: Arc<Central>,
    device_id: DeviceId,
    rssi: Option<i16>,
    handed_off: Arc<Mutex<HashSet<String>>>,
    in_flight: Arc<Mutex<HashSet<String>>>,
    connected: Arc<Mutex<HashSet<String>>>,
) {
    let id = device_id.as_str().to_string();
    let result = async {
        tokio::time::timeout(HANDOFF_CONNECT, central.connect(&device_id))
            .await
            .map_err(|_| anyhow::anyhow!("connect timeout"))??;
        tokio::time::timeout(HANDOFF_CONNECT, central.discover_services(&device_id))
            .await
            .map_err(|_| anyhow::anyhow!("service discovery timeout"))??;
        let bytes = tokio::time::timeout(
            HANDOFF_CONNECT,
            central.read_characteristic(&device_id, INVITE_CHAR_UUID),
        )
        .await
        .map_err(|_| anyhow::anyhow!("invite read timeout"))??;
        let invite = String::from_utf8(bytes)?.trim().to_string();
        if !invite.starts_with("mycena://") {
            anyhow::bail!("invalid invite payload");
        }
        let account = bridge::account_key_from_invite(&invite)
            .unwrap_or_else(|| device_id.as_str().to_string());
        if handed_off.lock().await.contains(&account) {
            return Ok(());
        }
        if let Some(peer) = bridge::peer_id_from_invite(&invite) {
            if !peer.is_empty() && ctx.handle.is_peer_connected(&peer).await {
                handed_off.lock().await.insert(account);
                ctx.links.register(&peer, device_id.as_str());
                bridge::register_ble_link(&ctx.handle, &peer, device_id.as_str()).await;
                connected.lock().await.insert(id.clone());
                return Ok(());
            }
        }
        bridge::apply_transport_invite(&ctx.app_node, &invite).await?;
        if let Some(peer) = bridge::peer_id_from_invite(&invite) {
            if !peer.is_empty() {
                ctx.links.register(&peer, device_id.as_str());
                bridge::register_ble_link(&ctx.handle, &peer, device_id.as_str()).await;
            }
        }
        handed_off.lock().await.insert(account);
        connected.lock().await.insert(id.clone());
        info!("BLE handoff ok device={} rssi={:?}", device_id, rssi);
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(e) = result {
        warn!("BLE handoff {device_id}: {e}");
        let _ = central.disconnect(&device_id).await;
        connected.lock().await.remove(&id);
    }
    in_flight.lock().await.remove(&id);
}

async fn flush_relay(
    ctx: &BleHostContext,
    central: &Central,
    connected: &Arc<Mutex<HashSet<String>>>,
) {
    let live_set: HashSet<String> = ctx.handle.known_peers().await.into_iter().collect();
    let ble_addrs = bridge::ble_peer_addrs(&ctx.handle).await;
    let items = bridge::drain_bearer_outbound(&ctx.handle, None, 16).await;
    for (peer, frame, medium) in items {
        if medium != mycena_l0_facade::bearer_transport::BearerMedium::Ble {
            continue;
        }
        if !should_send_over_ble(&peer, &live_set, &ble_addrs) {
            continue;
        }
        let device_id = match ctx.links.device_for_peer(&peer) {
            Some(d) => d,
            None => continue,
        };
        if !connected.lock().await.contains(&device_id) {
            continue;
        }
        let device = DeviceId::from(device_id.as_str());
        let _ = send_mesh_frame(central, &device, &frame).await;
    }
}

fn should_send_over_ble(peer: &str, connected: &HashSet<String>, ble_addrs: &[String]) -> bool {
    if !connected.contains(peer) {
        return true;
    }
    ble_addrs.iter().any(|a| a.contains(peer))
}

async fn send_mesh_frame(central: &Central, device: &DeviceId, frame: &[u8]) -> bool {
    for chunk in mycena_l0_facade::ble_mesh::chunk_frame(frame) {
        let ok = central
            .write_characteristic(device, MESH_DATA_CHAR_UUID, chunk, WriteType::WithResponse)
            .await
            .is_ok();
        if !ok {
            return false;
        }
        tokio::time::sleep(CHUNK_WRITE_DELAY).await;
    }
    true
}
