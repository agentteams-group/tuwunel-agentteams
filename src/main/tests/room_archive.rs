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
		[
			"logging.enable=false",
			"database.pool.max_workers=16",
			"server_name=\"archive.test\"",
			"admin.room.create=true",
			"registration.grant_admin_to_first_user=false",
			"rooms.task_room_managers=[\"@archive_manager:archive.test\"]",
			"rooms.task_room_admins=[\"@archive_l1:archive.test\"]",
		],
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
	for kind in [None, Some("dm"), Some("group_room"), Some("team_room")] {
		let protected = owner
			.create_room(&json!({"preset": "private_chat"}))
			.await?;
		if let Some(kind) = kind {
			owner
				.put(&format!("rooms/{protected}/state/room.meta/"), &json!({"roomKind": kind}))
				.await?;
		}
		assert_eq!(
			set_archive(&owner, &protected, "archive")
				.await?
				.status(),
			403
		);
		let response = owner
			.request(
				Method::PUT,
				&owner.url(&format!("rooms/{protected}/state/{ARCHIVE_STATE}/")),
				&json!({"archived": true}),
			)
			.await?;
		assert_eq!(response.status(), 403);
		owner
			.post(&format!("rooms/{protected}/leave"), &json!({}))
			.await?;
	}
	worker
		.post(&format!("rooms/{active_room}/join"), &json!({}))
		.await?;
	owner
		.put(
			&format!("rooms/{room}/state/room.meta/"),
			&json!({
				"roomKind": "task_room", "teamName": "archive-team",
				"teamAdmin": {"userId": owner_id}, "humanMembers": [{"userId": worker_id}]
			}),
		)
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
	let private_room = owner
		.create_room(&json!({"name": "Other archived room"}))
		.await?;
	owner
		.put(
			&format!("rooms/{private_room}/state/room.meta/"),
			&json!({"roomKind": "task_room", "leaderWorker": {"userId": owner_id}}),
		)
		.await?;
	assert_eq!(
		set_archive(&owner, &private_room, "archive")
			.await?
			.status(),
		200
	);
	let list_url = owner.client_url("", &format!("{ARCHIVE_PATH}/archived"));
	let listed = accepted(
		owner
			.request(Method::GET, &list_url, &json!({}))
			.await?,
	)
	.await?;
	assert_eq!(listed["rooms"].as_array().unwrap().len(), 2);
	for (caller, count) in [(&worker, 1), (&outsider, 0)] {
		let listed = accepted(
			caller
				.request(Method::GET, &list_url, &json!({}))
				.await?,
		)
		.await?;
		assert_eq!(listed["rooms"].as_array().unwrap().len(), count);
	}
	for query in [
		format!("team_admin={owner_id}"),
		format!("human_member={worker_id}"),
		"team=archive-team".to_owned(),
	] {
		let url = format!("{list_url}?{query}");
		let listed = accepted(
			owner
				.request(Method::GET, &url, &json!({}))
				.await?,
		)
		.await?;
		assert_eq!(listed["rooms"].as_array().unwrap().len(), 1);
		assert_eq!(listed["rooms"][0]["room_id"], room.as_str());
		assert_eq!(listed["rooms"][0]["can_archive"], true);
		// Supplying someone else's admin identity cannot grant access.
		let listed = accepted(
			outsider
				.request(Method::GET, &url, &json!({}))
				.await?,
		)
		.await?;
		assert_eq!(listed["rooms"], json!([]));
	}
	let url = format!("{list_url}?team_admin={worker_id}&human_member={worker_id}");
	assert_eq!(
		accepted(
			owner
				.request(Method::GET, &url, &json!({}))
				.await?
		)
		.await?["rooms"],
		json!([])
	);
	assert_eq!(
		owner
			.request(Method::GET, &format!("{list_url}?team_admin=invalid"), &json!({}))
			.await?
			.status(),
		400
	);
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
	assert_eq!(
		accepted(
			worker
				.request(Method::GET, &list_url, &json!({}))
				.await?
		)
		.await?["rooms"],
		json!([])
	);
	owner
		.put(
			&format!("rooms/{room}/send/m.room.message/after-restore"),
			&json!({"msgtype": "m.text", "body": "Continue revising the result"}),
		)
		.await?;
	assert!(
		messages(&worker, &room).await?["chunk"]
			.as_array()
			.unwrap()
			.iter()
			.any(|event| event["content"]["body"] == "Continue revising the result")
	);
	assert!(
		services
			.state_cache
			.is_joined(&owner_id, &room)
			.await
	);
	let delete_url = owner.client_url("", &format!("{ARCHIVE_PATH}/{room}/delete"));
	assert_eq!(
		owner
			.request(Method::POST, &delete_url, &json!({}))
			.await?
			.status(),
		403
	);
	let admin_id =
		register(services, "archive_admin", "archive-admin-access-token-20261116-0001").await?;
	services.admin.make_user_admin(&admin_id).await?;
	register(services, "archive_manager", "archive-manager-access-token-20261116-0001").await?;
	let admin = Client {
		services,
		base,
		token: "archive-admin-access-token-20261116-0001",
	};
	let manager = Client {
		services,
		base,
		token: "archive-manager-access-token-20261116-0001",
	};
	register(services, "archive_l1", "archive-l1-access-token-20261116-0001").await?;
	let l1 = Client {
		services,
		base,
		token: "archive-l1-access-token-20261116-0001",
	};
	for (allowed, id) in [
		(&owner, owner_id.as_str()),
		(&worker, worker_id.as_str()),
		(&admin, admin_id.as_str()),
		(&manager, "@archive_manager:archive.test"),
		(&l1, "@archive_l1:archive.test"),
	] {
		let power = if id == owner_id.as_str() {
			json!({})
		} else {
			json!({"users": {(id): 100}})
		};
		let target = owner
			.create_room(&json!({"preset": "public_chat", "power_level_content_override": power}))
			.await?;
		owner.put(&format!("rooms/{target}/state/room.meta/"), &json!({"roomKind": "task_room", "teamAdmin": {"userId": owner_id}, "leaderWorker": {"userId": worker_id}})).await?;
		allowed
			.post(&format!("rooms/{target}/join"), &json!({}))
			.await?;
		assert_eq!(
			set_archive(allowed, &target, "archive")
				.await?
				.status(),
			200
		);
		if id == worker_id.as_str() {
			let forged = worker.request(Method::PUT, &worker.url(&format!("rooms/{target}/state/room.meta/")), &json!({"roomKind": "task_room", "teamAdmin": {"userId": worker_id}, "leaderWorker": {"userId": worker_id}})).await?;
			assert_eq!(forged.status(), 403, "Leader cannot promote itself to Team admin");
			let url = owner.client_url("", &format!("{ARCHIVE_PATH}/{target}/delete"));
			assert_eq!(
				worker
					.request(Method::POST, &url, &json!({}))
					.await?
					.status(),
				403
			);
		}
		assert_eq!(
			set_archive(allowed, &target, "unarchive")
				.await?
				.status(),
			200
		);
	}
	let elevated = owner.create_room(&json!({"preset": "public_chat", "power_level_content_override": {"users": {worker_id.as_str(): 100}}})).await?;
	owner
		.put(
			&format!("rooms/{elevated}/state/room.meta/"),
			&json!({"roomKind": "task_room", "teamAdmin": {"userId": owner_id}}),
		)
		.await?;
	worker
		.post(&format!("rooms/{elevated}/join"), &json!({}))
		.await?;
	assert_eq!(
		set_archive(&worker, &elevated, "archive")
			.await?
			.status(),
		403,
		"power level alone is not a business role"
	);
	// AgentTeams' existing creator power override needs the legacy room version.
	let mut legacy = json!({"room_version": "12", "preset": "trusted_private_chat", "power_level_content_override": {"users": {owner_id.as_str(): 100}}});
	assert_eq!(
		owner
			.request(Method::POST, &owner.url("createRoom"), &legacy)
			.await?
			.status(),
		400
	);
	legacy["room_version"] = json!("10");
	let legacy_room = owner.create_room(&legacy).await?;
	owner
		.put(
			&format!("rooms/{legacy_room}/state/room.meta/"),
			&json!({"roomKind": "task_room", "teamAdmin": {"userId": owner_id}}),
		)
		.await?;
	assert_eq!(
		set_archive(&owner, &legacy_room, "archive")
			.await?
			.status(),
		200
	);
	assert_eq!(
		set_archive(&owner, &legacy_room, "unarchive")
			.await?
			.status(),
		200
	);
	for allowed in [&owner, &admin, &manager] {
		let target = owner
			.create_room(&json!({"preset": "public_chat"}))
			.await?;
		owner
			.put(
				&format!("rooms/{target}/state/room.meta/"),
				&json!({"roomKind": "task_room", "teamAdmin": {"userId": owner_id}}),
			)
			.await?;
		worker
			.post(&format!("rooms/{target}/join"), &json!({}))
			.await?;
		let url = owner.client_url("", &format!("{ARCHIVE_PATH}/{target}/delete"));
		assert_eq!(
			allowed
				.request(Method::POST, &url, &json!({}))
				.await?
				.status(),
			403
		);
		assert_eq!(
			set_archive(&owner, &target, "archive")
				.await?
				.status(),
			200
		);
		for denied in [&worker, &outsider] {
			assert_eq!(
				denied
					.request(Method::POST, &url, &json!({}))
					.await?
					.status(),
				403
			);
		}
		let deleted = accepted(
			allowed
				.request(Method::POST, &url, &json!({}))
				.await?,
		)
		.await?;
		assert_eq!(deleted["deleted"], true);
		assert_eq!(deleted["failed_to_kick_users"], json!([]));
		assert!(
			!services
				.state_cache
				.is_joined(&worker_id, &target)
				.await
		);
		assert!(services.metadata.is_banned(&target).await);
		assert_eq!(
			worker
				.request(
					Method::GET,
					&worker.url(&format!("rooms/{target}/messages?dir=b")),
					&json!({})
				)
				.await?
				.status(),
			403
		);
	}
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
					client.put(&format!("rooms/{room}/state/room.meta/"), &json!({"roomKind": "task_room", "teamAdmin": {"userId": format!("@archive_persist:{}", services.globals.server_name())}})).await?;
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
