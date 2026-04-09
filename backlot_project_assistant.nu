# Backlot Project Assistant

const script_dir = path self | path dirname
cd $script_dir

source ~/claude-pilot-env.nu

print "Starting Backlot Project Assistant..."
print ""

claude --dangerously-skip-permissions --append-system-prompt-file prompts/project_assistant.md "/bm8:new_assistant_session"
