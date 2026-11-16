# Room archive extension

Archive a finished task room (`room.meta.roomKind = "task_room"`) to stop delivering it through sync for **every
member on this homeserver**, while preserving membership and history access.
This is an AgentTeams extension, not a standard Matrix API.
Direct messages, ordinary groups and Team rooms cannot be archived, restored
or deleted through this extension. Room names are not used to infer their type.

## API

Authenticated requests with an empty JSON body:

```text
POST /_matrix/client/unstable/org.agentteams/rooms/{roomId}/archive
POST /_matrix/client/unstable/org.agentteams/rooms/{roomId}/unarchive
POST /_matrix/client/unstable/org.agentteams/rooms/{roomId}/delete
GET  /_matrix/client/unstable/org.agentteams/rooms/archived
```

Archive and unarchive return `200 {}` and are idempotent. They require both
an authorized business role and normal Matrix state-event authorization:
the caller must be joined and able to send `org.agentteams.room.archive`.
The same checks apply when writing that marker through the standard state API.

| Role | Archive / restore | Delete |
| --- | --- | --- |
| Global admin / L1 (ALE) | Yes | Yes |
| Manager | Yes | Yes |
| This task's Team admin | Yes | Yes |
| This task's Team Leader | Yes | No |
| Other humans / Workers / another team's admin or Leader | No | No |

The Team admin and Leader identities come from the existing `room.meta`
fields `teamAdmin.userId` and `leaderWorker.userId`. A room power level alone
does not grant a business role. A Leader cannot promote itself to Team admin
by changing the metadata. Existing rooms cannot change to or from `task_room`.

Tuwunel knows homeserver administrators but does not resolve AgentTeams
Controller roles. Configure trusted full Matrix IDs for platform L1 users and
Managers on the server; never infer these roles from a username or a request
parameter. Empty lists grant nobody additional access:

```toml
[global.rooms]
task_room_admins = ["@platform-admin:example.org"]
task_room_managers = ["@manager:example.org"]
```

These roles supplement the existing Matrix room permissions for archive and
restore; they do not automatically join users or change room power levels.

### Query and history

The list response is `{"rooms": [...]}`. Each entry contains `room_id`,
`name`, `joined_member_count`, `room_meta`, `can_archive` and `can_delete`.
Only the authenticated account's **joined archived task rooms** are listed.
Optional filters are ANDed and only narrow that accessible set:

```text
?team=design
?team_admin=%40alice%3Aexample.org
?human_member=%40bob%3Aexample.org
```

Filters match `room.meta.teamName`, `teamAdmin.userId`, and
`humanMembers[].userId`, respectively, reusing the task room metadata written
by TeamHarness. Missing legacy metadata does not guess a match. Supplying a
different account's ID never impersonates it or grants its room access.

Read messages, threads, state and attachments with the existing Matrix APIs,
on demand. Listing and viewing do not unarchive the room.

### Delete

Delete requires **both** `task_room` and an archived flag, checked under the
same room lock used by archive/restore. Only an admin, Manager or the task's
Team admin may delete; the Leader role alone is insufficient. It returns
`{"deleted":true,"failed_to_kick_users":[]}` after the existing local room
shutdown/purge operation and blocks the room against rejoining. This removes
local room history and aliases; it does not delete workspace artifacts,
independently stored media or copies on another federated homeserver. The
upstream homeserver administration APIs retain their own behavior.

Read the current flag through the existing state API:

```text
GET /_matrix/client/v3/rooms/{roomId}/state/org.agentteams.room.archive/
```

An archived room has `{"archived": true}` at the empty state key. Unarchiving
writes `{"archived": false}`. A missing state event means the room is active.
Existing state-event APIs can also update this event under the same permissions.

## Behavior

- Legacy `/sync` omits archived rooms from joined, invited, knocked and left
  room sections, independently of client filters.
- Sliding sync omits archived rooms from lists and explicit room subscriptions.
  Room-scoped account data, receipts and typing follow the selected window.
- Sync does not register per-room timeline, typing or receipt watchers for
  archived rooms. Local archive state writes wake parked sync requests so
  restoration does not require a rejoin or client restart. Other account-wide
  updates may still wake the account's shared sync loop.
- Memberships, room aliases, existing events, attachments and history permissions
  are preserved. Use the existing `/messages`, `/state` and event APIs to read
  history on demand; `joined_rooms` still includes the room.
- Archiving controls sync delivery. It does not ban members, freeze writes,
  delete data, or change another homeserver's behavior in a federated room.
  An already generated response may still be in flight when archive completes.

The caller should finish and verify artifact uploads before archiving. Task
status and artifact storage remain the responsibility of AgentTeams.
The Dashboard provides a separate archived-task list with account-role
filters, historical message viewing, restoration and permission-gated deletion.
Its archived conversation and thread composers are read only until restored.

## Upgrade compatibility

The feature branch is based on the upstream **v2.0.0** release tag
(`a3feae1a26ec764ee88606c2c68b659a9b5fba8d`), rather than a moving `main`.

This feature adds **no database tables, column families, schema version,
migration, message encoding or membership encoding**. Its only persisted data
is an ordinary custom Matrix state event stored through the existing writer.
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

```sh
RUST_TEST_THREADS=1 cargo test -p tuwunel --no-default-features --test room_archive
```

The integration tests cover owner and Worker sync, ordinary member and outsider
authorization, existing sliding-sync connections and explicit subscriptions,
unchanged historical events, retained membership, repeat archive, restoration
with an existing token, watcher exclusion, restore wake-up and database restart.
They also cover account-role query filters, outsider isolation, non-task room
rejection, all five allowed archive identities, Leader deletion denial,
metadata privilege escalation, elevated Workers without a business role,
authorized deletion and legacy room-version-10 creation/archive/restore.
Both tests passed locally on 2026-11-16. On this macOS host, Apple's linker
asserted on a generated symbol name; selecting its `-ld_classic` linker allowed
the tests to link and run. No linker configuration was added to the repository.
