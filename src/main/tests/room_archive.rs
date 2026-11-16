#![cfg(test)]

use std::{
	env::{current_exe, var},
	fs::remove_dir_all,
	path::PathBuf,
	process::Command,
	time::Duration,
};

use reqwest::Method;
use serde_json::{Value, json};
use tuwunel::Args;
use tuwunel_core::{Result, ruma::RoomId};
use tuwunel_service::Services;

use self::{
	client::{Client, accepted, register},
	fixture::{boot, boot_at},
};

mod client;
mod fixture;
mod sliding_sync;

#[path = "client/put.rs"]
mod put;

const ARCHIVE_PATH: &str = "unstable/org.agentteams/rooms";
const ARCHIVE_STATE: &str = "org.agentteams.room.archive";
const OWNER_TOKEN: &str = "archive-owner-access-token-20261116-0001";
const WORKER_TOKEN: &str = "archive-worker-access-token-20261116-0001";
const OUTSIDER_TOKEN: &str = "archive-outsider-access-token-20261116-0001";
const PERSIST_TOKEN: &str = "archive-persist-access-token-20261116-0001";

#[test]
fn room_archive_preserves_history_and_membership_for_every_member() -> Result {
	if var("TUWUNEL_ARCHIVE_TEST_PHASE").is_ok() {
		return Ok(());
	}

	boot(
		"room-archive",
		["logging.enable=false", "database.pool.max_workers=16"],
		exercise,
	)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let owner_id = register(services, "archive_owner", OWNER_TOKEN).await?;
	let worker_id = register(services, "archive_worker", WORKER_TOKEN).await?;
	register(services, "archive_outsider", OUTSIDER_TOKEN).await?;
	let owner = Client { services, base, token: OWNER_TOKEN };
	let worker = Client { services, base, token: WORKER_TOKEN };
	let outsider = Client { services, base, token: OUTSIDER_TOKEN };
	let room = owner
		.create_room(&json!({"preset": "public_chat"}))
		.await?;
	worker
		.post(&format!("rooms/{room}/join"), &json!({}))
		.await?;
	let active_room = owner
		.create_room(&json!({"preset": "public_chat"}))
		.await?;
	worker
		.post(&format!("rooms/{active_room}/join"), &json!({}))
		.await?;
	let original = owner
		.put(
			&format!("rooms/{room}/send/m.room.message/before-archive"),
			&json!({"msgtype": "m.text", "body": "Existing task result"}),
		)
		.await?;
	let before = messages(&worker, &room).await?;
	assert!(
		before["chunk"]
			.as_array()
			.unwrap()
			.iter()
			.any(|event| event["event_id"] == original["event_id"])
	);
	let worker_sync = sync(&worker, "").await?;
	assert!(
		worker_sync["rooms"]["join"]
			.get(room.as_str())
			.is_some()
	);
	let mut positions = Vec::new();
	for member in [&owner, &worker] {
		let response = member
			.sliding_sync(
				None,
				0,
				&json!({
					"conn_id": "archive",
					"lists": {"main": {"ranges": [[0, 10]], "required_state": [], "timeline_limit": 10}}
				}),
			)
			.await?;
		assert!(response["rooms"].get(room.as_str()).is_some(), "{response}");
		positions.push(response["pos"].as_str().unwrap().to_owned());
	}

	// A normal member and an outsider cannot change the room's archive state.
	for caller in [&worker, &outsider] {
		assert_eq!(
			set_archive(caller, &room, "archive")
				.await?
				.status(),
			403
		);
	}
	assert_eq!(
		set_archive(&owner, &room, "archive")
			.await?
			.status(),
		200
	);
	assert_eq!(
		set_archive(&owner, &room, "archive")
			.await?
			.status(),
		200
	);
	let state = get(&owner, &format!("rooms/{room}/state/{ARCHIVE_STATE}/")).await?;
	assert_eq!(state, json!({"archived": true}));
	assert_archived_watcher(services, &worker_id, &room).await?;

	for (member, position) in [&owner, &worker].into_iter().zip(&positions) {
		// No client-side filter: the server must hide the room for all members.
		for since in
			["".to_owned(), format!("&since={}", worker_sync["next_batch"].as_str().unwrap())]
		{
			let response = sync(member, &since).await?;
			if since.is_empty() {
				assert!(
					response["rooms"]["join"]
						.get(active_room.as_str())
						.is_some(),
					"active room was hidden: {response}"
				);
			}
			for section in ["join", "invite", "leave", "knock"] {
				assert!(
					response["rooms"][section]
						.get(room.as_str())
						.is_none(),
					"{response}"
				);
			}
		}
		let response = member
			.sliding_sync(
				Some(position),
				0,
				&json!({
					"conn_id": "archive",
					"lists": {"main": {"ranges": [[0, 10]], "required_state": [], "timeline_limit": 10}},
					"room_subscriptions": {room.as_str(): {"required_state": [], "timeline_limit": 10}}
				}),
			)
			.await?;
		assert!(response["rooms"].get(room.as_str()).is_none(), "{response}");
		assert_eq!(response["lists"]["main"]["count"], 1, "{response}");
		let joined = get(member, "joined_rooms").await?;
		assert!(
			joined["joined_rooms"]
				.as_array()
				.unwrap()
				.iter()
				.any(|id| id == room.as_str())
		);
		let history = messages(member, &room).await?;
		for event in before["chunk"].as_array().unwrap() {
			let actual = history["chunk"]
				.as_array()
				.unwrap()
				.iter()
				.find(|item| item["event_id"] == event["event_id"])
				.expect("old event is preserved");
			for field in
				["event_id", "type", "sender", "content", "state_key", "origin_server_ts"]
			{
				assert_eq!(actual[field], event[field], "history field {field} changed");
			}
		}
	}

	// Restoring does not need a rejoin or a new sync token.
	let archived_sync = sync(&worker, "").await?;
	let archive_watcher = services
		.sync
		.watch(&worker_id, None, services.state_cache.rooms_joined(&worker_id))
		.await;
	assert_eq!(
		set_archive(&owner, &room, "unarchive")
			.await?
			.status(),
		200
	);
	tokio::time::timeout(Duration::from_secs(1), archive_watcher)
		.await
		.expect("unarchive wakes a parked sync");
	let response =
		sync(&worker, &format!("&since={}", archived_sync["next_batch"].as_str().unwrap()))
			.await?;
	assert!(
		response["rooms"]["join"]
			.get(room.as_str())
			.is_some(),
		"{response}"
	);
	let state = get(&worker, &format!("rooms/{room}/state/{ARCHIVE_STATE}/")).await?;
	assert_eq!(state, json!({"archived": false}));
	assert!(
		services
			.state_cache
			.is_joined(&owner_id, &room)
			.await
	);
	Ok(())
}

async fn assert_archived_watcher(
	services: &Services,
	worker: &tuwunel_core::ruma::UserId,
	room: &RoomId,
) -> Result {
	let watcher = services
		.sync
		.watch(worker, None, services.state_cache.rooms_joined(worker))
		.await;
	services
		.typing
		.typing_update_sender
		.send(room.to_owned())
		.ok();
	assert!(
		tokio::time::timeout(Duration::from_millis(100), watcher)
			.await
			.is_err(),
		"archived room still has a typing watcher"
	);
	Ok(())
}

#[test]
fn room_archive_survives_restart() -> Result {
	if let Ok(phase) = var("TUWUNEL_ARCHIVE_TEST_PHASE") {
		let database = PathBuf::from(var("TUWUNEL_ARCHIVE_TEST_DATABASE").unwrap());
		let harnesses: &[&str] = if phase == "seed" { &["fresh"] } else { &[] };
		return boot_at(
			&database,
			harnesses,
			["logging.enable=false", "database.pool.max_workers=16"],
			async |services, base| {
				if phase == "seed" {
					register(services, "archive_persist", PERSIST_TOKEN).await?;
				}
				let client = Client { services, base, token: PERSIST_TOKEN };
				let room = if phase == "seed" {
					client
						.create_room(
							&json!({"room_alias_name": "archive-persistent", "preset": "private_chat"}),
						)
						.await?
				} else {
					let alias = get(
						&client,
						&format!(
							"directory/room/%23archive-persistent:{}",
							services.globals.server_name()
						),
					)
					.await?;
					alias["room_id"].as_str().unwrap().try_into()?
				};
				if phase == "seed" {
					assert!(
						sync(&client, "").await?["rooms"]["join"]
							.get(room.as_str())
							.is_some()
					);
					client
						.put(
							&format!("rooms/{room}/send/m.room.message/persist"),
							&json!({"msgtype": "m.text", "body": "Persisted result"}),
						)
						.await?;
					assert_eq!(
						set_archive(&client, &room, "archive")
							.await?
							.status(),
						200
					);
				} else {
					assert!(
						sync(&client, "").await?["rooms"]["join"]
							.get(room.as_str())
							.is_none()
					);
					assert!(
						messages(&client, &room).await?["chunk"]
							.as_array()
							.unwrap()
							.iter()
							.any(|event| event["content"]["body"] == "Persisted result")
					);
					assert_eq!(
						set_archive(&client, &room, "unarchive")
							.await?
							.status(),
						200
					);
					assert!(
						sync(&client, "").await?["rooms"]["join"]
							.get(room.as_str())
							.is_some()
					);
				}
				assert_eq!(services.globals.db.database_version().await, 17);
				Ok(())
			},
		);
	}
	let database = Args::test_database_path("room-archive-persistence");
	for phase in ["seed", "restart"] {
		let output = Command::new(current_exe()?)
			.env("TUWUNEL_ARCHIVE_TEST_PHASE", phase)
			.env("TUWUNEL_ARCHIVE_TEST_DATABASE", &database)
			.output()?;
		assert!(
			output.status.success(),
			"{phase}: {}\n{}",
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		);
	}
	remove_dir_all(database)?;
	Ok(())
}

async fn set_archive(
	client: &Client<'_>,
	room: &RoomId,
	action: &str,
) -> Result<reqwest::Response> {
	client
		.post_url(&client.client_url("", &format!("{ARCHIVE_PATH}/{room}/{action}")), &json!({}))
		.await
}

async fn sync(client: &Client<'_>, since: &str) -> Result<Value> {
	get(client, &format!("sync?timeout=0{since}")).await
}

async fn messages(client: &Client<'_>, room: &RoomId) -> Result<Value> {
	get(client, &format!("rooms/{room}/messages?dir=b&limit=100")).await
}

async fn get(client: &Client<'_>, path: &str) -> Result<Value> {
	accepted(
		client
			.request(Method::GET, &client.url(path), &json!({}))
			.await?,
	)
	.await
}
