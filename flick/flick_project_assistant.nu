# Flick Project Assistant

const script_dir = path self | path dirname
cd $script_dir

$env.PATH = ($env.PATH | prepend ([$script_dir, "target", "debug"] | path join))

print "Starting Flick Project Assistant..."
print 'Type "go" or press Enter to get a project status summary.'
print ""

claude --dangerously-skip-permissions --append-system-prompt "You are the Flick Project Assistant. Your first action: read prompts/project_assistant.md and follow the instructions there exactly. Treat ANY first message from the user (including empty, 'go', 'hi', etc.) as the trigger to execute your bootstrap instructions."
