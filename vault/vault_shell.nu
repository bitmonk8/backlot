# Vault shell

const script_dir = path self | path dirname

if "VAULT_SHELL" not-in $env {
    $env.VAULT_SHELL = "1"
    const self_path = path self
    ^nu --env-config $self_path
    exit
}

cd $script_dir

print "Ready."
