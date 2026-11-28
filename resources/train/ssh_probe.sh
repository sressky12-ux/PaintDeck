declare -A seen
probe() {
    local dir="$1" py version
    [[ "$dir" =~ ^/[a-zA-Z0-9/._~-]*$ && -f "$dir/run.py" ]] || return 1
    [[ -z "${seen[$dir]}" ]] || return 1
    seen[$dir]=1
    version=$(sed -nE "s/^[[:space:]]*VERSION[[:space:]]*=[[:space:]]*['\"]([0-9]+\.[0-9]+\.[0-9]+)['\"][[:space:]]*$/\1/p" "$dir/version.py" 2>/dev/null | head -1)
    if [[ "$dir" != "$preferred" ]]; then
        [[ -n "$version" && $(printf '%s\n' "$minimum" "$version" | sort -V | head -1) == "$minimum" ]] || return 1
    fi
    for py in "$dir/.venv/bin/python" "$dir/venv/bin/python" "$(command -v python3)" /root/miniconda3/bin/python; do
        [[ "$py" =~ ^/[a-zA-Z0-9/._~-]+$ && -x "$py" ]] || continue
        if (cd "$dir" && "$py" -c 'import torch,diffusers,sys; sys.exit(0 if torch.cuda.is_available() else 1)') >/dev/null 2>&1; then
            printf 'PAINTDECK_KIT\t%s\t%s\t%s\n' "$dir" "$py" "${version:-unknown}"
            return 0
        fi
    done
    return 1
}
for dir in "$preferred" /app/ai-toolkit /root/ai-toolkit /workspace/ai-toolkit "$HOME/ai-toolkit" "$root/ai-toolkit" "$root/ai-toolkit-$minimum"; do
    probe "$dir" && exit 0
done
while IFS= read -r -d '' file; do
    probe "${file%/run.py}" && exit 0
done < <(timeout 20 find / -maxdepth 4 -path /proc -prune -o -name run.py -path '*ai-toolkit*' -print0 2>/dev/null)
echo PAINTDECK_NONE
