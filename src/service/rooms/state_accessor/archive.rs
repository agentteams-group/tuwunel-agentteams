use ruma::{RoomId, UserId, events::StateEventType};
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

/// Business roles are independent from Matrix room power levels.
#[implement(super::Service)]
pub async fn can_delete_task_room(&self, user: &UserId, room: &RoomId) -> bool {
	if self.services.admin.user_is_admin(user).await
		|| self
			.services
			.config
			.rooms
			.task_room_admins
			.iter()
			.any(|id| id == user)
		|| self
			.services
			.config
			.rooms
			.task_room_managers
			.iter()
			.any(|id| id == user)
	{
		return true;
	}
	self.room_state_get_content(room, &StateEventType::from("room.meta"), "")
		.await
		.is_ok_and(|meta: Value| meta["teamAdmin"]["userId"].as_str() == Some(user.as_str()))
}

#[implement(super::Service)]
pub async fn can_archive_task_room(&self, user: &UserId, room: &RoomId) -> bool {
	if self.can_delete_task_room(user, room).await {
		return true;
	}
	self.room_state_get_content(room, &StateEventType::from("room.meta"), "")
		.await
		.is_ok_and(|meta: Value| meta["leaderWorker"]["userId"].as_str() == Some(user.as_str()))
}
