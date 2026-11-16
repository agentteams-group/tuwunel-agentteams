use axum::extract::State;
use ruma::{
	RoomId, UserId, api::client::membership::leave_room, events::StateEventType, serde::Raw,
};
use serde_json::json;
use tuwunel_core::Result;
use tuwunel_service::{Services, rooms::state_accessor::ROOM_ARCHIVE_EVENT_TYPE};

use crate::{Ruma, RumaResponse, state::send_state_event_for_key_helper};

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
