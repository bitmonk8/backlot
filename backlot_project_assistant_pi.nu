# Backlot Project Assistant (pi variant)
#
# Equivalent of backlot_project_assistant.nu using pi instead of claude.
# Requires: unity-pilot provider configured in pi.

const script_dir = path self | path dirname
cd $script_dir

print "Starting Backlot Project Assistant..."
print ""

pi --provider unity-pilot --model claude-opus-4-6 --append-system-prompt prompts/project_assistant.md /new-session
