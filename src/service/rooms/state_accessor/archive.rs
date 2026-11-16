use ruma::{
	RoomId,
	events::{RoomAccountDataEventType, StateEventType},
};
use serde_json::Value;
use tuwunel_core::implement;

/// Rooms without the optional archive state retain their normal sync behavior.
#[implement(super::Service)]
pub async fn is_room_archived(&self, room_id: &RoomId) -> bool {
	if let Ok(event) = self
		.services
		.account_data
		.get_room::<Value>(
			room_id,
			&self.services.globals.server_user,
			RoomAccountDataEventType::from(super::ROOM_ARCHIVE_EVENT_TYPE),
		)
		.await
	{
		return event["content"]["archived"].as_bool() == Some(true)
			&& self.is_task_room(room_id).await;
	}
	self.room_state_get_content(
		room_id,
		&StateEventType::from(super::ROOM_ARCHIVE_EVENT_TYPE),
		"",
	)
	.await
	.is_ok_and(|content: Value| content.get("archived").and_then(Value::as_bool) == Some(true))
		&& self.is_task_room(room_id).await
}

#[implement(super::Service)]
pub async fn is_task_room(&self, room_id: &RoomId) -> bool {
	self.room_state_get_content(room_id, &StateEventType::from("room.meta"), "")
		.await
		.is_ok_and(|content: Value| {
			content.get("roomKind").and_then(Value::as_str) == Some("task_room")
		})
}

#[implement(super::Service)]
pub async fn room_restore_count(&self, room: &RoomId) -> u64 {
	self.services
		.account_data
		.get_room::<Value>(
			room,
			&self.services.globals.server_user,
			RoomAccountDataEventType::from(super::ROOM_ARCHIVE_EVENT_TYPE),
		)
		.await
		.ok()
		.filter(|event| event["content"]["archived"] == false)
		.and_then(|event| event["content"]["count"].as_u64())
		.unwrap_or(0)
}
