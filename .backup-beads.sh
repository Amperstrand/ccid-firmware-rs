#!/bin/bash
# Backup all Gas Town rig bead databases
# Run via cron: 0 */6 * * * /home/ubuntu/gt/.backup-beads.sh >> /home/ubuntu/gt/.backup-beads.log 2>&1

GT_ROOT="/home/ubuntu/gt"
LOG_PREFIX="$(date '+%Y-%m-%d %H:%M:%S')"

# Find all rigs with .beads directories
for rig_dir in "$GT_ROOT"/*/; do
  rig=$(basename "$rig_dir")
  beads_dir="$rig_dir/.beads"
  
  # Skip if no .beads directory or no metadata.json
  [ ! -d "$beads_dir" ] && continue
  [ ! -f "$beads_dir/metadata.json" ] && continue
  
  # Skip if this is the town root .beads (symlink)
  [ "$rig" = ".beads-wisp" ] && continue
  
  # Run backup
  result=$(cd "$rig_dir" && bd backup 2>&1)
  exit_code=$?
  
  if [ $exit_code -eq 0 ]; then
    echo "$LOG_PREFIX ✓ $rig: $result"
  else
    echo "$LOG_PREFIX ✗ $rig: $result" >&2
  fi
done
