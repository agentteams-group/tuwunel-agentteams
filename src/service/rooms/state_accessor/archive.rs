use ruma::{RoomId, events::StateEventType};
use serde_json::Value;
use tuwunel_core::implement;

/// Rooms without the optional archive state retain their normal sync behavior.
#[implement(super::Service)]
pub async fn is_room_archived(&self, room_id: &RoomId) -> bool {
	self.room_state_get_content(
		room_id,
		&StateEventType::from(super::ROOM_ARCHIVE_EVENT_TYPE),
		"",
	)
	.await
	.is_ok_and(|content: Value| content.get("archived").and_then(Value::as_bool) == Some(true))
}
