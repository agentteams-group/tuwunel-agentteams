# Task room archive extension

Archive finished task rooms to stop `/sync` delivery for every local member,
while retaining membership, messages and history access. Only rooms explicitly
marked `room.meta.roomKind = "task_room"` are eligible. Direct messages, ordinary
groups and Team rooms cannot be archived or deleted through this extension.

## Authorization and call chain

Browser -> Dashboard API -> AgentTeams Controller -> Tuwunel.

Controller authenticates the real caller and resolves current Human, Manager,
Worker and Team CR permissions. The room's current owning Team is resolved by
`room.meta.teamName`, supporting the Team CR name/effective storage-name mapping.
The owning team and task-room kind cannot be changed through room state updates.

| Controller caller | Archive / restore | Delete |
| --- | --- | --- |
| Admin / L1 (ALE) | Yes | Yes |
| Manager | Yes | Yes |
| This task's current Team admin | Yes | Yes |
| This task's current Team Leader | Yes | No |
| Ordinary humans / Workers / another team's admin or Leader | No | No |

The Controller requires the current Team CR relationship, rather than trusting
client role parameters, room power levels or an old `teamAdmin` metadata field.
Lifecycle writes are audited using the authenticated caller's identity.

Tuwunel accepts lifecycle writes only from a homeserver administrator, whose
credential stays on the Controller. Normal Matrix tokens cannot bypass Controller
RBAC, even if the token belongs to a room creator or has power level 100.
Controller calls can archive/restore without joining that administrative account
to the task room or modifying anybody's power level. No static L1/Manager account
allowlists are needed in Tuwunel.

## APIs

Controller APIs (authenticated with the existing Controller/Human bearer token):

```text
GET  /api/v1/task-rooms/archived
GET  /api/v1/task-rooms/{roomId}/archive
POST /api/v1/task-rooms/{roomId}/archive
POST /api/v1/task-rooms/{roomId}/unarchive
POST /api/v1/task-rooms/{roomId}/delete
```

The corresponding Tuwunel extension lives at
`/_matrix/client/unstable/org.agentteams/rooms`. POST operations are restricted
to the homeserver administrator. GET room status is available to that service
identity and joined members. The ordinary room-state archive event is reserved;
writing it directly is rejected. A member's own room account data cannot change
the server-owned archive flag.

The list returns `{"rooms": [...]}` with `room_id`, `name`,
`joined_member_count`, `room_meta`, `can_archive` and `can_delete`. It starts from
the account's joined archived task rooms and is narrowed by Controller team
access. Optional ANDed filters reuse the existing TeamHarness metadata:

```text
?team=design
?team_admin=%40alice%3Aexample.org
?human_member=%40bob%3Aexample.org
```

They match `room.meta.teamName`, `teamAdmin.userId`, and
`humanMembers[].userId`. A filtered account is not impersonated and does not
grant additional room access. Missing metadata does not guess a match.
Room status returns `is_task_room`, `archived`, `can_archive` and `can_delete`;
the Dashboard uses the Controller's capabilities rather than deriving roles.

Archive and unarchive return `200 {}` and can safely be repeated. The archive
flag is stored as room account data belonging to the homeserver's internal
server user, using existing storage and counters. Unarchive's counter forces a
fresh room baseline in both legacy and Sliding Sync, including after restart.
No ordinary Matrix history event or membership event is changed by archiving.

## Behavior and deletion

- Legacy sync omits archived joined, invited, knocked and left rooms.
- Sliding sync omits archived list entries and explicit subscriptions, including
  room-scoped extensions. Archived rooms have no per-room sync watchers.
- Lifecycle changes wake parked sync requests. Restoration needs no rejoin or
  new account login. An already generated response may still be in flight.
- The archived-room list and history are fetched on demand. Viewing history
  does not restore sync. Normal Matrix history and attachment permissions apply.
- The Dashboard keeps archived conversations and threads read only. Restore
  before sending follow-up instructions or edits.
- Archiving does not freeze writes from arbitrary third-party Matrix clients,
  ban members, or change another homeserver's behavior in a federated room.
- Delete rechecks `task_room` and the archived flag under the same room lock
  used by archive/restore, then uses the existing local shutdown/purge operation
  and blocks the room against rejoining. Its result is
  `{"deleted":true,"failed_to_kick_users":[]}`. Workspace artifacts and
  independently stored media are retained; other homeservers' copies are outside
  this server's control. Upstream general administration APIs retain their own
  behavior.

Finish task acceptance and verify workspace artifact uploads before archiving.
Task completion and artifact storage remain AgentTeams responsibilities.

## Upgrade compatibility

The feature branch is based on the upstream **v2.0.0** release tag
(`a3feae1a26ec764ee88606c2c68b659a9b5fba8d`), rather than a moving `main`.

This feature adds **no database tables, column families, schema version,
migration, message encoding or membership encoding**. Its only persisted data
is a server-owned room account-data record stored in the existing account-data maps. A previous feature-branch room-state marker is read as a fallback until the first lifecycle operation replaces it.
Rooms created before installing this feature have no flag and behave as before.
Archive state persists across restarts.

Replacing an unmodified binary built from the **same upstream revision** can
reuse its database without a feature-specific migration. An unmodified binary
ignores the custom event and resumes ordinary sync delivery; it does not erase
the event. This statement does not guarantee that upgrading or downgrading
between different upstream Tuwunel versions is migration-free. Follow the
upstream database upgrade requirements for those changes.

### AgentTeams client compatibility

Upstream 2.0 defaults new rooms to room version 12. Version 12 creators have
infinite power and cannot appear in `m.room.power_levels.users`
([Matrix specification](https://spec.matrix.org/v1.16/rooms/v12/)). The current
AgentTeams `roomflow` task-room creator supplies its own user at level 100;
that legacy override fails with the new default. Preserve room version 10 for
new rooms until the AgentTeams clients have been migrated:

```toml
[global.rooms]
default_version = "10"
```

This does not rewrite existing rooms or their data. The archive extension is
tested with the new version's implicit creator permissions as well. A successful
database upgrade alone does not validate every existing client request.

Upstream 1.2.0 and 2.0.0 both declare schema version 17, but 2.0.0 also runs
named migrations even at the same schema version. Its older flat configuration
names and `CONDUWUIT_` environment prefix remain supported. Preserve the server
name, database and media paths, appservice registration and credentials. Review
the [2.0 release notes](https://github.com/matrix-construct/tuwunel/releases/tag/v2.0.0-assets)
for new build defaults, stricter configuration validation and the media-upload
free-space threshold. A return to an old upstream version should use a backup
from before the upgrade, rather than assuming the upgraded database is reversible.

An isolated Docker exercise on 2026-11-16 opened an official 1.2.0 data volume
with official 2.0.0, keeping the original `CONDUWUIT_` configuration. Both users'
old access tokens, password logins, display names, memberships, historical
messages, custom state and uploaded media passed before and after replacement;
the upgraded server also accepted a new message in the existing room. This
small fixture does not validate a production database, encryption, federation
or every migration path. The 2.0 startup log performed migrations and removed
obsolete column families, despite the schema number remaining 17.

Allow the first-boot migrations to finish before applying normal liveness
timeouts. A deployment that probes the listening port immediately needs a
startup grace period for a large existing database.

## Validation

Run `RUST_TEST_THREADS=1 cargo test -p tuwunel --no-default-features --test room_archive`.
The HTTP fixture verifies service-only writes, task-only eligibility, account filters,
member isolation, member-owned account-data spoofing, v3/v5 sync exclusion and
restoration, watcher behavior, unchanged history, retained membership, authorized
purge, room-version-10 compatibility and database restart. Controller separately
tests the full business-role matrix and current Team ownership.
