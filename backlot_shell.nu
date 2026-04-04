# Backlot shell

const script_dir = path self | path dirname

if "BACKLOT_SHELL" not-in $env {
    $env.BACKLOT_SHELL = "1"
    const self_path = path self
    ^nu --env-config $self_path
    exit
}

cd $script_dir

source ~/claude-pilot-env.nu

print "Ready."
