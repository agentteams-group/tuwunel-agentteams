use axum::{Json, extract::State, http::Uri};
use futures::StreamExt;
use ruma::{
	OwnedUserId, RoomId, UserId,
	api::client::membership::{joined_rooms, leave_room},
	events::StateEventType,
	serde::Raw,
};
use serde::Deserialize;
use serde_json::json;
use tuwunel_core::{Err, Result, err};
use tuwunel_service::{Services, rooms::state_accessor::ROOM_ARCHIVE_EVENT_TYPE};

use crate::{Ruma, RumaResponse, state::send_state_event_for_key_helper};

#[derive(Default, Deserialize)]
pub(crate) struct ArchivedRoomsQuery {
	team: Option<String>,
	team_admin: Option<OwnedUserId>,
	human_member: Option<OwnedUserId>,
}

/// List only the caller's joined archived rooms, optionally narrowed by task room metadata.
pub(crate) async fn archived_rooms_route(
	State(services): State<crate::State>,
	uri: Uri,
	body: Ruma<joined_rooms::v3::Request>,
) -> Result<Json<serde_json::Value>> {
	let query: ArchivedRoomsQuery = serde_html_form::from_str(uri.query().unwrap_or_default())
		.map_err(|_| err!(Request(InvalidParam("Invalid archived room filters."))))?;
	let sender = body.sender_user();
	let joined: Vec<_> = services
		.state_cache
		.rooms_joined(sender)
		.map(ToOwned::to_owned)
		.collect()
		.await;
	let mut rooms = Vec::new();
	for room in joined {
		if !services
			.state_accessor
			.is_room_archived(&room)
			.await
		{
			continue;
		}
		let meta = services
			.state_accessor
			.room_state_get_content::<serde_json::Value>(
				&room,
				&StateEventType::from("room.meta"),
				"",
			)
			.await
			.unwrap_or_default();
		if query
			.team
			.as_deref()
			.is_some_and(|team| meta["teamName"].as_str() != Some(team))
			|| query
				.team_admin
				.as_deref()
				.is_some_and(|user| meta["teamAdmin"]["userId"].as_str() != Some(user.as_str()))
			|| query.human_member.as_deref().is_some_and(|user| {
				!meta["humanMembers"]
					.as_array()
					.is_some_and(|members| {
						members
							.iter()
							.any(|member| member["userId"].as_str() == Some(user.as_str()))
					})
			}) {
			continue;
		}
		let can_archive = services
			.state_accessor
			.get_power_levels(&room)
			.await?
			.user_can_send_state(sender, StateEventType::from(ROOM_ARCHIVE_EVENT_TYPE))
			&& services
				.state_accessor
				.can_archive_task_room(sender, &room)
				.await;
		rooms.push(json!({
			"room_id": room,
			"name": services.state_accessor.get_name(&room).await.ok(),
			"joined_member_count": services.state_cache.room_joined_count(&room).await?,
			"room_meta": meta,
			"can_archive": can_archive,
			"can_delete": services.state_accessor.can_delete_task_room(sender, &room).await,
		}));
	}
	Ok(Json(json!({"rooms": rooms})))
}

/// Permanently delete only an archived task room, using server-controlled roles.
pub(crate) async fn delete_archived_room_route(
	State(services): State<crate::State>,
	body: Ruma<leave_room::v3::Request>,
) -> Result<Json<serde_json::Value>> {
	let room = &body.room_id;
	let sender = body.sender_user();
	// Hold the same lock used by archive/unarchive through the final purge.
	let lock = services.state.mutex.lock(room).await;
	if !services
		.state_accessor
		.is_room_archived(room)
		.await
	{
		return Err!(Request(Forbidden("Only archived task rooms can be deleted.")));
	}
	if !services
		.state_accessor
		.can_delete_task_room(sender, room)
		.await
	{
		return Err!(Request(Forbidden(
			"Only an admin, Manager or this task's Team admin can delete it."
		)));
	}
	let summary = services
		.delete
		.delete_room(room, false, lock)
		.await?;
	services.metadata.block_room(room, sender);
	services.sync.notify_room_archive_changed();
	Ok(Json(
		json!({"deleted": true, "failed_to_kick_users": summary.failed_to_kick_users}),
	))
}

/// Archive a room for every member without leaving it or changing history access.
pub(crate) async fn archive_room_route(
	State(services): State<crate::State>,
	body: Ruma<leave_room::v3::Request>,
) -> Result<RumaResponse<leave_room::v3::Response>> {
	set_archived(&services, body.sender_user(), &body.room_id, true).await?;
	Ok(RumaResponse(leave_room::v3::Response::new()))
}

/// Restore normal room sync for every member.
pub(crate) async fn unarchive_room_route(
	State(services): State<crate::State>,
	body: Ruma<leave_room::v3::Request>,
) -> Result<RumaResponse<leave_room::v3::Response>> {
	set_archived(&services, body.sender_user(), &body.room_id, false).await?;
	Ok(RumaResponse(leave_room::v3::Response::new()))
}

async fn set_archived(
	services: &Services,
	sender: &UserId,
	room: &RoomId,
	archived: bool,
) -> Result {
	// Use the ordinary state-event writer for membership, power levels and deduplication.
	send_state_event_for_key_helper(
		services,
		sender,
		room,
		&StateEventType::from(ROOM_ARCHIVE_EVENT_TYPE),
		&Raw::new(&json!({"archived": archived}))?.cast_unchecked(),
		"",
		None,
	)
	.await?;
	Ok(())
}
