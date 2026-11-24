// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use crate::contacts::{Contact, ContactStatus};
use crate::envelope::{BulletinPost, ChatMessage, MailMessage};
use crate::groups::Group;
use crate::scope_key::ScopeKey;
use mycena_l0_facade::crdt::{ContactDocument, ContactStatusWire, GroupDocument, HlcClock};
use mycena_l0_facade::data::now_ms;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AppStorage {
    db: sled::Db,
    contacts: sled::Tree,
    contact_accounts: sled::Tree,
    groups: sled::Tree,
    mail_inbox: sled::Tree,
    mail_sent: sled::Tree,
    mail_read: sled::Tree,
    /// When set, mini-app KV values are encrypted at rest (Cell C2).
    at_rest_key: Option<[u8; 32]>,
}

impl AppStorage {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        Self::open_with_key(path, None)
    }

    pub fn open_with_key(path: &str, at_rest_key: Option<[u8; 32]>) -> anyhow::Result<Self> {
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()?;
        Ok(Self {
            contacts: db.open_tree("contacts:data")?,
            contact_accounts: db.open_tree("contacts:accounts")?,
            groups: db.open_tree("groups:data")?,
            mail_inbox: db.open_tree("mail:inbox")?,
            mail_sent: db.open_tree("mail:sent")?,
            mail_read: db.open_tree("mail:read")?,
            db,
            at_rest_key,
        })
    }

    pub fn save_chat(&self, peer_id: &str, msg: &ChatMessage) -> anyhow::Result<()> {
        let tree = self.db.open_tree(format!("chat:{peer_id}"))?;
        tree.insert(ts_key(msg.timestamp_ms), bincode::serialize(msg)?)?;
        Ok(())
    }

    pub fn chat_history(&self, peer_id: &str, limit: usize) -> anyhow::Result<Vec<ChatMessage>> {
        let tree = self.db.open_tree(format!("chat:{peer_id}"))?;
        let mut out = Vec::new();
        for item in tree.iter().rev().take(limit) {
            let (_k, v) = item?;
            out.push(bincode::deserialize(&v)?);
        }
        out.reverse();
        Ok(out)
    }

    pub fn save_contact(&self, contact: &Contact) -> anyhow::Result<()> {
        let tree = self.contacts.clone();
        tree.insert(contact.peer_id.as_bytes(), bincode::serialize(contact)?)?;
        Ok(())
    }

    pub fn contact_by_id(&self, peer_id: &str) -> anyhow::Result<Option<Contact>> {
        let tree = self.contacts.clone();
        Ok(match tree.get(peer_id.as_bytes())? {
            Some(v) => Some(bincode::deserialize(&v)?),
            None => None,
        })
    }

    pub fn contact_by_account_id(&self, account_id: &str) -> anyhow::Result<Option<Contact>> {
        let idx = self.contact_accounts.clone();
        let Some(peer_bytes) = idx.get(account_id.as_bytes())? else {
            return Ok(None);
        };
        let peer_id = String::from_utf8_lossy(&peer_bytes).to_string();
        self.contact_by_id(&peer_id)
    }

    pub fn all_contacts(&self) -> anyhow::Result<Vec<Contact>> {
        let tree = self.contacts.clone();
        let mut out: Vec<Contact> = Vec::new();
        for item in tree.iter() {
            let (_k, v) = item?;
            match bincode::deserialize::<Contact>(&v) {
                Ok(c) => out.push(c),
                Err(e) => tracing::warn!("skipping corrupt contact entry: {e}"),
            }
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.added_at_ms));
        Ok(out)
    }

    pub fn contacts_with_status(&self, status: ContactStatus) -> anyhow::Result<Vec<Contact>> {
        Ok(self
            .all_contacts()?
            .into_iter()
            .filter(|c| c.status == status)
            .collect())
    }

    pub fn delete_contact(&self, peer_id: &str) -> anyhow::Result<()> {
        let tree = self.contacts.clone();
        tree.remove(peer_id.as_bytes())?;
        Ok(())
    }

    pub fn upsert_contact(
        &self,
        peer_id: &str,
        display_name: &str,
        status: ContactStatus,
    ) -> anyhow::Result<Contact> {
        let actor = self.crdt_actor_id()?;
        let ts = self.next_hlc_timestamp()?;
        let contact = match self.contact_by_id(peer_id)? {
            Some(mut existing) => {
                existing.ensure_crdt(&actor);
                existing.apply_crdt_update(&actor, ts, |doc| {
                    if !display_name.is_empty() {
                        doc.set_display_name(display_name.to_string(), ts);
                    }
                    doc.set_status(ContactStatusWire::from(status), ts);
                });
                existing
            }
            None => {
                let mut c = Contact::new(peer_id.to_string(), display_name.to_string(), status);
                c.ensure_crdt(&actor);
                c.apply_crdt_update(&actor, ts, |doc| {
                    doc.set_display_name(display_name.to_string(), ts);
                    doc.set_status(ContactStatusWire::from(status), ts);
                });
                c
            }
        };
        self.save_contact(&contact)?;
        Ok(contact)
    }

    pub fn upsert_contact_with_account(
        &self,
        peer_id: &str,
        display_name: &str,
        status: ContactStatus,
        account_id: Option<&str>,
    ) -> anyhow::Result<Contact> {
        let mut contact = self.upsert_contact(peer_id, display_name, status)?;
        if let Some(aid) = account_id.filter(|s| !s.is_empty()) {
            contact.account_id = Some(aid.to_string());
            self.save_contact(&contact)?;
            let idx = self.contact_accounts.clone();
            idx.insert(aid.as_bytes(), peer_id.as_bytes())?;
        }
        Ok(contact)
    }

    pub fn is_peer_blocked(&self, peer_id: &str) -> anyhow::Result<bool> {
        Ok(self
            .contact_by_id(peer_id)?
            .map(|c| c.status == ContactStatus::Blocked)
            .unwrap_or(false))
    }

    /// Canonical peer id first, then recent transport ids (SD-190 transition window).
    pub fn transport_peer_candidates(&self, peer_id: &str) -> anyhow::Result<Vec<String>> {
        let Some(contact) = self.contact_by_id(peer_id)? else {
            return Ok(vec![peer_id.to_string()]);
        };
        let mut out = vec![contact.peer_id.clone()];
        for prev in contact.previous_peer_ids.iter().rev() {
            if !out.iter().any(|p| p == prev) {
                out.push(prev.clone());
            }
        }
        if !out.iter().any(|p| p == peer_id) {
            out.push(peer_id.to_string());
        }
        Ok(out)
    }

    /// Update stored peer id when a contact rotates transport identity (SD-190).
    pub fn remap_contact_peer(
        &self,
        account_id: &str,
        new_peer_id: &str,
    ) -> anyhow::Result<Option<Contact>> {
        let idx = self.contact_accounts.clone();
        let old_peer_id = match idx.get(account_id.as_bytes())? {
            Some(v) => String::from_utf8_lossy(&v).to_string(),
            None => return Ok(None),
        };
        if old_peer_id == new_peer_id {
            return self.contact_by_id(new_peer_id);
        }
        let Some(mut contact) = self.contact_by_id(&old_peer_id)? else {
            return Ok(None);
        };
        if old_peer_id != new_peer_id && !contact.previous_peer_ids.contains(&old_peer_id) {
            contact.previous_peer_ids.push(old_peer_id.clone());
            while contact.previous_peer_ids.len() > 8 {
                contact.previous_peer_ids.remove(0);
            }
        }
        let actor = self.crdt_actor_id()?;
        contact.ensure_crdt(&actor);
        if let Some(doc) = contact.crdt.as_mut() {
            doc.note_previous_peer(old_peer_id.clone());
        }
        contact.sync_from_crdt();
        let tree = self.contacts.clone();
        tree.remove(old_peer_id.as_bytes())?;
        contact.peer_id = new_peer_id.to_string();
        contact.account_id = Some(account_id.to_string());
        tree.insert(new_peer_id.as_bytes(), bincode::serialize(&contact)?)?;
        idx.insert(account_id.as_bytes(), new_peer_id.as_bytes())?;
        if old_peer_id != new_peer_id {
            self.remap_chat_tree(&old_peer_id, new_peer_id)?;
        }
        Ok(Some(contact))
    }

    fn remap_chat_tree(&self, old_peer_id: &str, new_peer_id: &str) -> anyhow::Result<()> {
        let old_name = format!("chat:{old_peer_id}");
        let new_name = format!("chat:{new_peer_id}");
        if !self
            .db
            .tree_names()
            .iter()
            .any(|n| n.as_ref() == old_name.as_bytes())
        {
            return Ok(());
        }
        let old_tree = self.db.open_tree(&old_name)?;
        let new_tree = self.db.open_tree(&new_name)?;
        for item in old_tree.iter() {
            let (k, v) = item?;
            new_tree.insert(k, v)?;
        }
        self.db.drop_tree(old_name)?;
        Ok(())
    }

    /// Replace a member's transport peer id in all groups (SD-190 rotation follow).
    pub fn group_remap_member_peer(
        &self,
        old_peer_id: &str,
        new_peer_id: &str,
    ) -> anyhow::Result<usize> {
        if old_peer_id == new_peer_id {
            return Ok(0);
        }
        let tree = self.groups.clone();
        let mut keys = Vec::new();
        for item in tree.iter() {
            let (k, _) = item?;
            keys.push(k.to_vec());
        }
        let mut updated = 0usize;
        for key in keys {
            let Some(v) = tree.get(&key)? else {
                continue;
            };
            let mut group: crate::groups::Group = bincode::deserialize(&v)?;
            let mut changed = false;
            for member in group.members.iter_mut() {
                if member == old_peer_id {
                    *member = new_peer_id.to_string();
                    changed = true;
                }
            }
            if changed {
                tree.insert(key, bincode::serialize(&group)?)?;
                updated += 1;
            }
        }
        Ok(updated)
    }

    pub fn save_bulletin(&self, post: &BulletinPost) -> anyhow::Result<()> {
        let tree = self.db.open_tree(format!("bulletin:{}", post.scope))?;
        let key = format!("{:020}:{}", post.expires_at_ms, post.id);
        tree.insert(key.as_bytes(), bincode::serialize(post)?)?;
        Ok(())
    }

    pub fn bulletins_for_scope(&self, scope: &str) -> anyhow::Result<Vec<BulletinPost>> {
        let tree = self.db.open_tree(format!("bulletin:{scope}"))?;
        let mut out = Vec::new();
        let now = now_ms();
        for item in tree.iter() {
            let (_k, v) = item?;
            let post: BulletinPost = bincode::deserialize(&v)?;
            if post.expires_at_ms >= now {
                out.push(post);
            }
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.timestamp_ms));
        Ok(out)
    }

    pub fn prune_expired_bulletins(&self) -> anyhow::Result<usize> {
        let now = now_ms();
        let mut removed = 0usize;
        for name in self.db.tree_names() {
            let Some(name) = std::str::from_utf8(&name).ok() else {
                continue;
            };
            if !name.starts_with("bulletin:") {
                continue;
            }
            let tree = self.db.open_tree(name)?;
            let cutoff = format!("{now:020}:");
            let mut keys = Vec::new();
            for item in tree.range(..cutoff.as_bytes()) {
                let (k, _) = item?;
                keys.push(k.to_vec());
            }
            for k in keys {
                tree.remove(k)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn save_mail_inbox(&self, mail: &MailMessage) -> anyhow::Result<()> {
        let tree = self.mail_inbox.clone();
        tree.insert(
            ts_uuid_key(mail.timestamp_ms, mail.id),
            bincode::serialize(mail)?,
        )?;
        Ok(())
    }

    pub fn save_mail_sent(&self, mail: &MailMessage) -> anyhow::Result<()> {
        let tree = self.mail_sent.clone();
        tree.insert(
            ts_uuid_key(mail.timestamp_ms, mail.id),
            bincode::serialize(mail)?,
        )?;
        Ok(())
    }

    pub fn inbox(&self, limit: usize) -> anyhow::Result<Vec<MailMessage>> {
        let tree = self.mail_inbox.clone();
        newest_mails(&tree, limit)
    }

    pub fn sent(&self, limit: usize) -> anyhow::Result<Vec<MailMessage>> {
        let tree = self.mail_sent.clone();
        newest_mails(&tree, limit)
    }

    pub fn mark_read(&self, mail_id: &Uuid) -> anyhow::Result<()> {
        let tree = self.mail_read.clone();
        tree.insert(mail_id.to_string().as_bytes(), &[1])?;
        Ok(())
    }

    pub fn is_read(&self, mail_id: &Uuid) -> anyhow::Result<bool> {
        let tree = self.mail_read.clone();
        Ok(tree.get(mail_id.to_string().as_bytes())?.is_some())
    }

    pub fn save_group(&self, group: &Group) -> anyhow::Result<()> {
        let tree = self.groups.clone();
        tree.insert(group.id.as_bytes(), bincode::serialize(group)?)?;
        Ok(())
    }

    pub fn delete_group(&self, id: &str) -> anyhow::Result<()> {
        let tree = self.groups.clone();
        tree.remove(id.as_bytes())?;
        Ok(())
    }

    pub fn group_by_id(&self, id: &str) -> anyhow::Result<Option<Group>> {
        let tree = self.groups.clone();
        Ok(match tree.get(id.as_bytes())? {
            Some(v) => Some(bincode::deserialize(&v)?),
            None => None,
        })
    }

    pub fn all_groups(&self) -> anyhow::Result<Vec<Group>> {
        let tree = self.groups.clone();
        let mut out = Vec::new();
        for item in tree.iter() {
            let (_k, v) = item?;
            out.push(bincode::deserialize(&v)?);
        }
        Ok(out)
    }

    /// Records a libp2p peer id as having been seen in this group (OR-Set membership).
    pub fn group_record_peer_seen(&self, group_id: &str, peer_id: &str) -> anyhow::Result<()> {
        let Some(mut g) = self.group_by_id(group_id)? else {
            return Ok(());
        };
        let actor = self.crdt_actor_id()?;
        g.record_peer_seen_crdt(peer_id.to_string(), &actor);
        self.save_group(&g)
    }

    pub fn merge_contact_document(&self, remote: ContactDocument) -> anyhow::Result<Contact> {
        let actor = self.crdt_actor_id()?;
        let mut clock = self.load_hlc_clock()?;
        let contact = match self.contact_by_id(&remote.peer_id)? {
            Some(mut local) => {
                local.ensure_crdt(&actor);
                if let Some(doc) = local.crdt.as_mut() {
                    doc.merge(&remote, &mut clock);
                }
                local.sync_from_crdt();
                local
            }
            None => {
                let mut c = Contact::new(
                    remote.peer_id.clone(),
                    remote.materialized_display_name().to_string(),
                    remote.materialized_status().into(),
                );
                c.crdt = Some(remote);
                c.sync_from_crdt();
                c
            }
        };
        self.save_hlc_clock(&clock)?;
        self.save_contact(&contact)?;
        Ok(contact)
    }

    pub fn merge_group_document(&self, remote: GroupDocument) -> anyhow::Result<Group> {
        let actor = self.crdt_actor_id()?;
        let mut clock = self.load_hlc_clock()?;
        let group = match self.group_by_id(&remote.id)? {
            Some(mut local) => {
                local.ensure_crdt(&actor);
                if let Some(doc) = local.crdt.as_mut() {
                    doc.merge(&remote, &mut clock);
                }
                local.sync_from_crdt();
                local
            }
            None => {
                let mut g = Group {
                    id: remote.id.clone(),
                    name: remote.materialized_name().to_string(),
                    key: remote.key,
                    members: remote.materialized_members(),
                    created_at_ms: remote.materialized_created_at_ms(),
                    crdt: Some(remote),
                };
                g.sync_from_crdt();
                g
            }
        };
        self.save_hlc_clock(&clock)?;
        self.save_group(&group)?;
        Ok(group)
    }

    pub fn crdt_actor_id(&self) -> anyhow::Result<String> {
        let tree = self.db.open_tree("meta:crdt")?;
        if let Some(v) = tree.get(b"actor_id")? {
            return Ok(String::from_utf8_lossy(&v).to_string());
        }
        let id = Uuid::new_v4().to_string();
        tree.insert(b"actor_id", id.as_bytes())?;
        Ok(id)
    }

    pub fn load_hlc_clock(&self) -> anyhow::Result<HlcClock> {
        let tree = self.db.open_tree("meta:crdt")?;
        Ok(match tree.get(b"hlc_clock")? {
            Some(v) => bincode::deserialize(&v)?,
            None => HlcClock::new(&self.crdt_actor_id()?),
        })
    }

    fn save_hlc_clock(&self, clock: &HlcClock) -> anyhow::Result<()> {
        let tree = self.db.open_tree("meta:crdt")?;
        tree.insert(b"hlc_clock", bincode::serialize(clock)?)?;
        Ok(())
    }

    pub fn next_hlc_timestamp(&self) -> anyhow::Result<mycena_l0_facade::crdt::HlcTimestamp> {
        let mut clock = self.load_hlc_clock()?;
        let ts = clock.tick(now_ms());
        self.save_hlc_clock(&clock)?;
        Ok(ts)
    }

    pub fn save_marketplace_stock(
        &self,
        stock: &mycena_l0_facade::crdt::MarketplaceStock,
    ) -> anyhow::Result<()> {
        let tree = self.db.open_tree("marketplace:stock")?;
        tree.insert(stock.listing_id.as_bytes(), bincode::serialize(stock)?)?;
        Ok(())
    }

    pub fn marketplace_stock(
        &self,
        listing_id: &str,
    ) -> anyhow::Result<Option<mycena_l0_facade::crdt::MarketplaceStock>> {
        let tree = self.db.open_tree("marketplace:stock")?;
        Ok(match tree.get(listing_id.as_bytes())? {
            Some(v) => Some(bincode::deserialize(&v)?),
            None => None,
        })
    }

    pub fn merge_marketplace_stock(
        &self,
        remote: mycena_l0_facade::crdt::MarketplaceStock,
    ) -> anyhow::Result<mycena_l0_facade::crdt::MarketplaceStock> {
        let actor = self.crdt_actor_id()?;
        let mut local = self
            .marketplace_stock(&remote.listing_id)?
            .unwrap_or_else(|| {
                mycena_l0_facade::crdt::MarketplaceStock::new(remote.listing_id.clone(), 0, &actor)
            });
        local.merge(&remote);
        self.save_marketplace_stock(&local)?;
        Ok(local)
    }

    pub fn save_group_chat(&self, group_id: &str, msg: &ChatMessage) -> anyhow::Result<()> {
        let tree = self.db.open_tree(format!("groupchat:{group_id}"))?;
        tree.insert(
            ts_uuid_key(msg.timestamp_ms, msg.id),
            bincode::serialize(msg)?,
        )?;
        Ok(())
    }

    pub fn group_chat_history(
        &self,
        group_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let tree = self.db.open_tree(format!("groupchat:{group_id}"))?;
        let mut out = Vec::new();
        for item in tree.iter().rev().take(limit) {
            let (_k, v) = item?;
            out.push(bincode::deserialize(&v)?);
        }
        out.reverse();
        Ok(out)
    }

    /// Mini-app bridge KV (`app:<app_id>:<user_key>`) in the main app sled DB.
    pub fn miniapp_get(&self, scoped_key: &str) -> anyhow::Result<Option<String>> {
        let tree = self.db.open_tree("miniapp_storage")?;
        Ok(match tree.get(scoped_key.as_bytes())? {
            Some(v) => Some(self.decode_miniapp_value(scoped_key, &v)?),
            None => None,
        })
    }

    pub fn miniapp_set(&self, scoped_key: &str, value: &str) -> anyhow::Result<()> {
        let tree = self.db.open_tree("miniapp_storage")?;
        let bytes = self.encode_miniapp_value(scoped_key, value)?;
        tree.insert(scoped_key.as_bytes(), bytes)?;
        Ok(())
    }

    fn encode_miniapp_value(&self, scoped_key: &str, value: &str) -> anyhow::Result<Vec<u8>> {
        match &self.at_rest_key {
            Some(master) => crate::miniapp_storage_enc::encrypt_value(master, scoped_key, value),
            None => Ok(value.as_bytes().to_vec()),
        }
    }

    fn decode_miniapp_value(&self, scoped_key: &str, blob: &[u8]) -> anyhow::Result<String> {
        match &self.at_rest_key {
            Some(master) => crate::miniapp_storage_enc::decrypt_value(master, scoped_key, blob),
            None => Ok(String::from_utf8_lossy(blob).into_owned()),
        }
    }

    pub fn miniapp_delete(&self, scoped_key: &str) -> anyhow::Result<()> {
        let tree = self.db.open_tree("miniapp_storage")?;
        tree.remove(scoped_key.as_bytes())?;
        Ok(())
    }

    pub fn miniapp_list(&self, scoped_prefix: &str) -> anyhow::Result<Vec<String>> {
        let tree = self.db.open_tree("miniapp_storage")?;
        let mut out = Vec::new();
        for item in tree.scan_prefix(scoped_prefix.as_bytes()) {
            let (k, _) = item?;
            out.push(String::from_utf8_lossy(&k).into_owned());
        }
        out.sort();
        Ok(out)
    }

    pub fn miniapp_clear_all_for_app(&self, app_id: &str) -> anyhow::Result<()> {
        let prefix = format!("app:{app_id}:");
        let tree = self.db.open_tree("miniapp_storage")?;
        let keys: Vec<Vec<u8>> = tree
            .scan_prefix(prefix.as_bytes())
            .filter_map(|r| r.ok())
            .map(|(k, _)| k.to_vec())
            .collect();
        for k in keys {
            tree.remove(k)?;
        }
        Ok(())
    }

    pub fn save_scope_key(&self, sk: &ScopeKey) -> anyhow::Result<()> {
        let tree = self.db.open_tree("scope_keys")?;
        tree.insert(sk.scope.as_bytes(), serde_json::to_vec(sk)?.as_slice())?;
        Ok(())
    }

    pub fn get_scope_key(&self, scope: &str) -> anyhow::Result<Option<ScopeKey>> {
        let tree = self.db.open_tree("scope_keys")?;
        Ok(match tree.get(scope.as_bytes())? {
            Some(bytes) => Some(serde_json::from_slice(&bytes)?),
            None => None,
        })
    }

    pub fn all_scope_keys(&self) -> anyhow::Result<Vec<ScopeKey>> {
        let tree = self.db.open_tree("scope_keys")?;
        let mut keys = Vec::new();
        for item in tree.iter() {
            let (_, v) = item?;
            if let Ok(sk) = serde_json::from_slice::<ScopeKey>(&v) {
                keys.push(sk);
            }
        }
        keys.sort_by_key(|sk| sk.added_at_ms);
        Ok(keys)
    }

    pub fn delete_scope_key(&self, scope: &str) -> anyhow::Result<()> {
        let tree = self.db.open_tree("scope_keys")?;
        tree.remove(scope.as_bytes())?;
        Ok(())
    }
}

fn newest_mails(tree: &sled::Tree, limit: usize) -> anyhow::Result<Vec<MailMessage>> {
    let mut out = Vec::new();
    for item in tree.iter().rev().take(limit) {
        let (_k, v) = item?;
        out.push(bincode::deserialize(&v)?);
    }
    Ok(out)
}

fn ts_key(ts: u64) -> [u8; 8] {
    ts.to_be_bytes()
}

fn ts_uuid_key(ts: u64, id: Uuid) -> Vec<u8> {
    format!("{ts:020}:{id}").into_bytes()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn miniapp_storage_encryption_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let key = blake3::derive_key("test-miniapp-enc", b"seed");
        let storage = AppStorage::open_with_key(dir.path().to_str().unwrap(), Some(key)).unwrap();
        storage
            .miniapp_set("app:com.enc.app:secret", "payload")
            .unwrap();
        let v = storage.miniapp_get("app:com.enc.app:secret").unwrap();
        assert_eq!(v.as_deref(), Some("payload"));
    }

    #[test]
    fn miniapp_storage_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let storage = AppStorage::open(dir.path().to_str().unwrap()).unwrap();

        storage
            .miniapp_set("app:com.app.a:key1", "value_a")
            .unwrap();
        storage
            .miniapp_set("app:com.app.b:key1", "value_b")
            .unwrap();

        let a_keys = storage.miniapp_list("app:com.app.a:").unwrap();
        assert_eq!(a_keys.len(), 1);
        assert!(a_keys[0].contains("com.app.a"));

        storage.miniapp_clear_all_for_app("com.app.a").unwrap();
        let b_val = storage.miniapp_get("app:com.app.b:key1").unwrap();
        assert_eq!(b_val.as_deref(), Some("value_b"));
    }

    #[test]
    fn group_without_crdt_sled_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = AppStorage::open(dir.path().to_str().unwrap()).unwrap();
        let g = Group::new("Team".into());
        storage.save_group(&g).unwrap();
        let loaded = storage.group_by_id(&g.id).unwrap().expect("exists");
        assert_eq!(loaded.name, "Team");
    }

    #[test]
    fn group_crdt_bincode_roundtrip() {
        let mut g = Group::new("Team".into());
        g.record_peer_seen_crdt("12D3KooWpeer".into(), "local-actor");
        let bytes = bincode::serialize(&g).expect("serialize");
        let loaded: Group = bincode::deserialize(&bytes).expect("deserialize");
        assert!(loaded.members.contains(&"12D3KooWpeer".to_string()));
    }

    #[test]
    fn marketplace_stock_merge_surfaces_oversell_conflict() {
        use mycena_l0_facade::crdt::{MarketplaceStock, StockQuantity};

        let dir = tempfile::tempdir().unwrap();
        let storage = AppStorage::open(dir.path().to_str().unwrap()).unwrap();
        let actor = storage.crdt_actor_id().unwrap();

        let mut local = MarketplaceStock::new("listing-1".into(), 1, &actor);
        local.record_sale(&actor, 1);
        storage.save_marketplace_stock(&local).unwrap();

        let mut remote = MarketplaceStock::new("listing-1".into(), 0, "peer-b");
        remote.record_sale("peer-b", 1);
        let merged = storage.merge_marketplace_stock(remote).unwrap();
        assert!(matches!(merged.quantity(), StockQuantity::Conflict(_)));
    }
}
