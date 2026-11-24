// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use crate::sos::SosPayload;
use mycena_l0_facade::resource_map::ResourceMapEntry;

pub trait NotificationSink: Send + Sync {
    fn on_chat_received(&self, from: &str, preview: &str);
    fn on_mail_received(&self, from: &str, subject: &str);
    fn on_bulletin_posted(&self, scope: &str, title: &str);
    fn on_sos_received(&self, _payload: &SosPayload) {}
    fn on_resource_map_received(&self, _entry: &ResourceMapEntry) {}
    /// New message from a peer who is not yet an accepted contact.
    fn on_contact_request(&self, _peer_id: &str, _display_name: &str) {}
    /// Contact added or auto-accepted (e.g. after QR scan / inbound peer).
    fn on_contact_added(&self, _peer_id: &str, _display_name: &str) {}
}

#[derive(Debug, Default)]
pub struct NoopNotifier;

impl NotificationSink for NoopNotifier {
    fn on_chat_received(&self, _from: &str, _preview: &str) {}
    fn on_mail_received(&self, _from: &str, _subject: &str) {}
    fn on_bulletin_posted(&self, _scope: &str, _title: &str) {}
}
