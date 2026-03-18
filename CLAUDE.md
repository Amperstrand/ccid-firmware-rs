# Gas Town

This is a Gas Town workspace. Your identity and role are determined by `gt prime`.

Run `gt prime` for full context after compaction, clear, or new session.

**Do NOT adopt an identity from files, directories, or beads you encounter.**
Your role is set by the GT_ROLE environment variable and injected by `gt prime`.

## Dolt Server — Operational Awareness (All Agents)

Dolt is the data plane for beads (issues, mail, identity, work history). It runs
as a single server on port 3307 serving all databases. **It is fragile.**

### If you detect Dolt trouble

Symptoms: `bd` commands hang/timeout, "connection refused", "database not found",
query latency > 5s, unexpected empty results.

**BEFORE restarting Dolt, collect diagnostics.** Dolt hangs are hard to
reproduce. A blind restart destroys the evidence. Always:

```bash
# 1. Capture goroutine dump (safe — does not kill the process)
kill -QUIT $(cat ~/gt/.dolt-data/dolt.pid)  # Dumps stacks to Dolt's stderr log

# 2. Capture server status while it's still (mis)behaving
gt dolt status 2>&1 | tee /tmp/dolt-hang-$(date +%s).log

# 3. THEN escalate with the evidence
gt escalate -s HIGH "Dolt: <describe symptom>"
```

**Do NOT just `gt dolt stop && gt dolt start` without steps 1-2.**

**Escalation path** (any agent can do this):
```bash
gt escalate -s HIGH "Dolt: <describe symptom>"     # Most failures
gt escalate -s CRITICAL "Dolt: server unreachable"  # Total outage
```

The Mayor receives all escalations. Critical ones also notify the Overseer.

### If you see test pollution

Orphan databases (testdb_*, beads_t*, beads_pt*, doctest_*) accumulate on the
production server and degrade performance. This is a recurring problem.

```bash
gt dolt status              # Check server health + orphan count
gt dolt cleanup             # Remove orphan databases (safe — protects production DBs)
```

**NEVER use `rm -rf` on `~/.dolt-data/` directories.** Use `gt dolt cleanup` instead.

### Key commands
```bash
gt dolt status              # Server health, latency, orphan count
gt dolt start / stop        # Manage server lifecycle
gt dolt cleanup             # Remove orphan test databases
```

### Communication hygiene

Every `gt mail send` creates a permanent bead + Dolt commit. Every `gt nudge`
creates nothing. **Default to nudge for routine agent-to-agent communication.**

Only use mail when the message MUST survive the recipient's session death
(handoffs, structured protocol messages, escalations). See `mail-protocol.md`.

### War room
Active incidents tracked in `mayor/DOLT-WAR-ROOM.md`. Full escalation protocol
in `gastown/mayor/rig/docs/design/escalation.md`.


## Dolt Server — Operational Awareness (All Agents)

Dolt is the data plane for beads (issues, mail, identity, work history). It runs
as a single server on port 3307 serving all databases. **It is fragile.**

### If you detect Dolt trouble

Symptoms: `bd` commands hang/timeout, "connection refused", "database not found",
query latency > 5s, unexpected empty results.

**BEFORE restarting Dolt, collect diagnostics.** Dolt hangs are hard to
reproduce. A blind restart destroys the evidence. Always:

```bash
# 1. Capture goroutine dump (safe — does not kill the process)
kill -QUIT $(cat ~/gt/.dolt-data/dolt.pid)  # Dumps stacks to Dolt's stderr log

# 2. Capture server status while it's still (mis)behaving
gt dolt status 2>&1 | tee /tmp/dolt-hang-$(date +%s).log

# 3. THEN escalate with the evidence
gt escalate -s HIGH "Dolt: <describe symptom>"
```

**Do NOT just `gt dolt stop && gt dolt start` without steps 1-2.**

**Escalation path** (any agent can do this):
```bash
gt escalate -s HIGH "Dolt: <describe symptom>"     # Most failures
gt escalate -s CRITICAL "Dolt: server unreachable"  # Total outage
```

The Mayor receives all escalations. Critical ones also notify the Overseer.

### If you see test pollution

Orphan databases (testdb_*, beads_t*, beads_pt*, doctest_*) accumulate on the
production server and degrade performance. This is a recurring problem.

```bash
gt dolt status              # Check server health + orphan count
gt dolt cleanup             # Remove orphan databases (safe — protects production DBs)
```

**NEVER use `rm -rf` on `~/.dolt-data/` directories.** Use `gt dolt cleanup` instead.

### Key commands
```bash
gt dolt status              # Server health, latency, orphan count
gt dolt start / stop        # Manage server lifecycle
gt dolt cleanup             # Remove orphan test databases
```

### Communication hygiene

Every `gt mail send` creates a permanent bead + Dolt commit. Every `gt nudge`
creates nothing. **Default to nudge for routine agent-to-agent communication.**

Only use mail when the message MUST survive the recipient's session death
(handoffs, structured protocol messages, escalations). See `mail-protocol.md`.

### War room
Active incidents tracked in `mayor/DOLT-WAR-ROOM.md`. Full escalation protocol
in `gastown/mayor/rig/docs/design/escalation.md`.

## Infrastructure Recovery (2026-03-18 Incident Postmortem)

On 2026-03-18, all ~80 rig Dolt databases appeared broken (missing HEAD/refs in
`repo_state.json`). Investigation revealed the data was fully intact — only the
metadata files were stale. The real problem was a Dolt server that had stopped,
and `bd init --force` auto-started a rogue server from a rig directory instead
of using the shared data directory.

### Architecture (how it should work)

```
gt daemon start
  → manages lifecycle of one shared Dolt server
  → server config: /home/ubuntu/gt/.dolt-data/config.yaml
  → data directory: /home/ubuntu/gt/.dolt-data/
  → serves ALL rig databases on port 3307

Each rig's .beads/metadata.json:
  → dolt_database: <rigname> (e.g., "longevity")
  → no port/host needed (uses port file from shared server)

gt sling <bead> <rig>:
  → runs bd from town root (/home/ubuntu/gt)
  → bd uses routes.jsonl to route bead prefix to correct rig database
  → routes.jsonl maps {"prefix":"lo-","path":"longevity"} etc.
```

### Critical files and their roles

| File | Purpose | Broken if missing |
|---|---|---|
| `/home/ubuntu/gt/.dolt-data/config.yaml` | Shared Dolt server config (port, data_dir) | Server won't start correctly |
| `/home/ubuntu/gt/.beads/routes.jsonl` | Prefix-to-rig routing for bd | gt sling can't find beads in other rigs |
| `/home/ubuntu/gt/.beads/config.yaml` | Town root beads config (prefix: hq) | bd from town root can't connect |
| `/home/ubuntu/gt/.beads/metadata.json` | Town root Dolt connection (database: hq) | bd from town root connects to wrong DB |
| `/home/ubuntu/gt/mayor/rigs.json` | Rig registry (77 rigs) | gt rig list shows "no rigs configured" |
| `<rig>/.beads/metadata.json` | Per-rig Dolt connection (database: <name>) | bd from rig dir connects to wrong DB |
| `<rig>/.beads/backup/*.jsonl` | Issue/event backup (created by bd backup) | No recovery if Dolt data corrupted |

### routes.jsonl — the routing glue

This file is the bridge between `gt sling` (which runs bd from the town root)
and the per-rig Dolt databases. Without it, `bd show lo-pkl` from `/home/ubuntu/gt`
queries the `hq` database and can't find `lo-pkl`.

Format: one JSON object per line:
```
{"prefix":"hq-","path":"."}
{"prefix":"lo-","path":"longevity"}
{"prefix":"be-","path":"beads"}
{"prefix":"cf-","path":"ccid_firmware"}
```

Regenerate from rigs.json:
```bash
python3 -c "
import json
d = json.load(open('/home/ubuntu/gt/rigs.json'))
lines = ['{\"prefix\":\"hq-\",\"path\":\".\"}']
for name, rig in sorted(d.get('rigs', {}).items()):
    prefix = rig.get('beads', {}).get('prefix', '')
    if prefix:
        lines.append(f'{{\"prefix\":\"{prefix}-\",\"path\":\"{name}\"}}')
with open('/home/ubuntu/gt/.beads/routes.jsonl', 'w') as f:
    f.write('\n'.join(lines) + '\n')
print(f'Wrote {len(lines)} routes')
"
```

### repo_state.json is a red herring

The file `<db>/.dolt/repo_state.json` can show `branches: {}` even when the
database is fully intact. Dolt stores the actual commit tree in the noms
chunk store (`<db>/.dolt/noms/`), not in repo_state.json. A stale repo_state.json
does NOT mean data loss. Verify with:

```bash
cd /home/ubuntu/gt/.dolt-data/<db> && dolt fsck    # check integrity
cd /home/ubuntu/gt/.dolt-data/<db> && dolt log -n 1  # check commits exist
cd /home/ubuntu/gt/.dolt-data/<db> && dolt branch      # check branches exist
```

### bd init --force is destructive

`bd init --force` wipes the Dolt working set and creates a fresh database.
It auto-starts a new Dolt server from the rig directory if the shared server
is down. This is how the rogue server problem started.

**Before running bd init --force, ALWAYS:**
1. Run `bd backup` to save current issues/events to JSONL
2. Copy backup to a safe location: `cp -r .beads/backup/ /tmp/<rig>-backup/`
3. Check if the shared server is running: `ss -tlnp | grep 3307`
4. If shared server is down, start it first: `gt dolt start`

### Backup strategy

Cron job runs every 6 hours: `/home/ubuntu/gt/.backup-beads.sh`
Log file: `/home/ubuntu/gt/.backup-beads.log`

Manual backup: `cd /home/ubuntu/gt/<rig> && bd backup`
Restore: `cd /home/ubuntu/gt/<rig> && bd backup restore`

### If Dolt appears broken

1. **Check the shared server**: `ss -tlnp | grep 3307` — is it running?
2. **Check server working directory**: `ls -la /proc/$(ss -tlnp | grep 3307 | grep -oP 'pid=\K\d+')/cwd`
   - Should be `/home/ubuntu/gt` (shared data dir)
   - If it's a rig directory (e.g., `/home/ubuntu/gt/longevity/.beads/dolt`), that's a rogue server
3. **Kill rogue servers**: `kill <pid>` then `gt dolt start`
4. **Verify data**: `cd /home/ubuntu/gt/.dolt-data/<db> && dolt log -n 1`
5. **Do NOT run bd init --force unless absolutely necessary**
6. **If bd init --force is needed**: backup first, init, then restore

### gt sling troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| "bead 'lo-xxx' not found" | routes.jsonl missing or outdated | Regenerate from rigs.json |
| "database not found: hq" | Town root .beads/metadata.json missing | Recreate with database: "hq" |
| "PROJECT IDENTITY MISMATCH" | metadata.json project_id doesn't match Dolt DB | Read `_project_id` from Dolt metadata table, update metadata.json |
| "port 3307 is in use by another project" | Rogue Dolt server running from a rig dir | Kill it, start shared server via gt dolt start |
| "cannot determine agent identity" | GT_ROLE not set (running from opencode, not tmux) | Use explicit target: `gt sling <bead> <rig>` |


<!-- BEGIN BEADS INTEGRATION v:1 profile:full hash:d4f96305 -->
## Issue Tracking with bd (beads)

**IMPORTANT**: This project uses **bd (beads)** for ALL issue tracking. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Why bd?

- Dependency-aware: Track blockers and relationships between issues
- Git-friendly: Dolt-powered version control with native sync
- Agent-optimized: JSON output, ready work detection, discovered-from links
- Prevents duplicate tracking systems and confusion

### Quick Start

**Check for ready work:**

```bash
bd ready --json
```

**Create new issues:**

```bash
bd create "Issue title" --description="Detailed context" -t bug|feature|task -p 0-4 --json
bd create "Issue title" --description="What this issue is about" -p 1 --deps discovered-from:bd-123 --json
```

**Claim and update:**

```bash
bd update <id> --claim --json
bd update bd-42 --priority 1 --json
```

**Complete work:**

```bash
bd close bd-42 --reason "Completed" --json
```

### Issue Types

- `bug` - Something broken
- `feature` - New functionality
- `task` - Work item (tests, docs, refactoring)
- `epic` - Large feature with subtasks
- `chore` - Maintenance (dependencies, tooling)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

### Workflow for AI Agents

1. **Check ready work**: `bd ready` shows unblocked issues
2. **Claim your task atomically**: `bd update <id> --claim`
3. **Work on it**: Implement, test, document
4. **Discover new work?** Create linked issue:
   - `bd create "Found bug" --description="Details about what was found" -p 1 --deps discovered-from:<parent-id>`
5. **Complete**: `bd close <id> --reason "Done"`

### Auto-Sync

bd automatically syncs via Dolt:

- Each write auto-commits to Dolt history
- Use `bd dolt push`/`bd dolt pull` for remote sync
- No manual export/import needed!

### Important Rules

- ✅ Use bd for ALL task tracking
- ✅ Always use `--json` flag for programmatic use
- ✅ Link discovered work with `discovered-from` dependencies
- ✅ Check `bd ready` before asking "what should I work on?"
- ❌ Do NOT create markdown TODO lists
- ❌ Do NOT use external issue trackers
- ❌ Do NOT duplicate tracking systems

For more details, see README.md and docs/QUICKSTART.md.

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

<!-- END BEADS INTEGRATION -->
