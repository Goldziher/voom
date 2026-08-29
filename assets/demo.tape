# voom's README demo. Record with:
#
#   scripts/demo-fixture.sh /tmp/voom-demo/projects
#   vhs assets/demo.tape
#
# The fixture script is committed beside this so the recording is reproducible — the previous
# GIF was made ad hoc and could not be regenerated.

Output assets/demo.gif

Set Shell "bash"
Set FontSize 22
Set Width 1560
Set Height 450
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 35ms

Hide
Type "cd /tmp/voom-demo && export PATH=$HOME/workspace/voom/target/release:$PATH && clear" Enter
Sleep 1s
Show

# Preview. Nothing is touched.
Type "voom --dry-run projects" Sleep 400ms Enter
Sleep 4s

Hide
Type "clear" Enter
Sleep 500ms
Show

# The same sweep for real: the dry run is this pipeline with the last step withheld.
Type "voom projects" Sleep 400ms Enter
Sleep 4s

# And the `target/` no Cargo.toml proved is still there.
Type "ls projects/notes/target" Sleep 400ms Enter
Sleep 3s
