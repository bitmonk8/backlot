# Flick Project Assistant

const script_dir = path self | path dirname
cd $script_dir

source ~/claude-pilot-env.nu

$env.PATH = ($env.PATH | prepend ([$script_dir, "target", "debug"] | path join))

print "Starting Flick Project Assistant..."
print ""

claude --dangerously-skip-permissions --append-system-prompt-file prompts/project_assistant.md "/new_assistant_session"
