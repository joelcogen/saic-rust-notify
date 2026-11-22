use super::Session;
use saic_client::MessageId;
use std::collections::{BTreeSet, VecDeque};

pub const CHECK_INTERVAL: i64 = 60;
const PAGE_SIZE: u32 = 20;
const MAX_PAGES: u32 = 3;
const RECENT_IDS: usize = (2 * MAX_PAGES * PAGE_SIZE) as usize;

#[derive(Debug)]
pub enum InboxError {
    SessionExpired,
    Unavailable,
}

#[derive(Default)]
pub struct Inbox {
    pub next_check: i64,
    seen: Option<VecDeque<MessageId>>,
}

impl Inbox {
    pub async fn check<'a>(
        &mut self,
        client: &mut impl Session,
        vins: &'a [String],
    ) -> Result<BTreeSet<&'a str>, InboxError> {
        let mut new_ids = Vec::new();
        let mut affected = BTreeSet::new();
        // The inbox is newest first. Stop at a known ID so deletions do not
        // make older entries further down the inbox look new.
        'pages: for page in 1..=MAX_PAGES {
            let messages = client.alarms(page, PAGE_SIZE).await?;
            let full = messages.len() >= PAGE_SIZE as usize;
            for message in messages.into_iter().take(PAGE_SIZE as usize) {
                let Some(id) = message.message_id else {
                    continue;
                };
                if self.seen.as_ref().is_some_and(|seen| seen.contains(&id)) {
                    break 'pages;
                }
                if new_ids.contains(&id) {
                    continue;
                }
                new_ids.push(id);
                if self.seen.is_some() {
                    if let Some(vin) = vins.iter().find(|vin| Some(*vin) == message.vin.as_ref()) {
                        affected.insert(vin.as_str());
                    }
                }
            }
            // At startup one page is enough to establish the current inbox head.
            if !full || self.seen.is_none() {
                break;
            }
        }
        let seen = self.seen.get_or_insert_default();
        for id in new_ids.into_iter().rev() {
            seen.push_front(id);
        }
        seen.truncate(RECENT_IDS);
        Ok(affected)
    }
}
