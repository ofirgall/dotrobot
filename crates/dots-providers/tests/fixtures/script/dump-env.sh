# Install script: records its runtime environment into $DOTS_STATE/env.txt.
pwd > "$DOTS_STATE/cwd.txt"
env | grep '^DOTS_' | sort > "$DOTS_STATE/env.txt"
if read -r line; then echo "stdin-had-data" > "$DOTS_STATE/stdin.txt"; else echo "stdin-eof" > "$DOTS_STATE/stdin.txt"; fi
echo "installing"
touch "$DOTS_STATE/installed"
