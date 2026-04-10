# setup_labels.nu — Delete default GitHub labels and create project taxonomy labels.
# Idempotent: safe to re-run.
# Usage: nu scripts/setup_labels.nu

let default_labels = [
  "bug"
  "documentation"
  "duplicate"
  "enhancement"
  "good first issue"
  "help wanted"
  "invalid"
  "question"
  "wontfix"
]

let project_labels = [
  # Crate labels
  { name: "crate:flick",         color: "1f77b4" }
  { name: "crate:lot",           color: "2ca02c" }
  { name: "crate:reel",          color: "17becf" }
  { name: "crate:vault",         color: "9467bd" }
  { name: "crate:epic",          color: "ff7f0e" }
  { name: "crate:mech",          color: "d62728" }
  # Importance labels
  { name: "importance:low",      color: "f9d0c4" }
  { name: "importance:medium",   color: "e99695" }
  { name: "importance:high",     color: "d73a4a" }
  # Effort labels
  { name: "effort:low",          color: "c2e0c6" }
  { name: "effort:medium",       color: "91ca55" }
  { name: "effort:high",         color: "0e8a16" }
  # Type labels
  { name: "type:bug",            color: "d4c5f9" }
  { name: "type:testing",        color: "bfd4f2" }
  { name: "type:security",       color: "f9c513" }
  { name: "type:performance",    color: "fbca04" }
  { name: "type:complexity",     color: "c5def5" }
  { name: "type:naming",         color: "e6e6e6" }
  { name: "type:docs",           color: "d4c5f9" }
  { name: "type:placement",      color: "f0e68c" }
]

# Phase 1: Delete default labels
print "Deleting default GitHub labels..."
for label in $default_labels {
  print -e $"  Deleting ($label)..."
  try {
    ^gh label delete $label --yes
  } catch {
    print -e $"  ($label) not found, skipping."
  }
}

# Phase 2: Create project labels
print "Creating project labels..."
for label in $project_labels {
  print -e $"  Creating ($label.name)..."
  ^gh label create $label.name --color $label.color --force
}

print $"Done. ($project_labels | length) labels created."
