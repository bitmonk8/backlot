# Lot shell

const script_dir = path self | path dirname

if "LOT_SHELL" not-in $env {
    $env.LOT_SHELL = "1"
    const self_path = path self
    ^nu --env-config $self_path
    exit
}

cd $script_dir

source ~/claude-pilot-env.nu

print "Ready."
