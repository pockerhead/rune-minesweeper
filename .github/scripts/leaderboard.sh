#!/usr/bin/env bash
# Checks every open leaderboard submission (issues labelled `score`) with verify-score: a newly
# accepted run is committed to main, then the issue gets the verifier's comment, a label, and
# is closed. Run by .github/workflows/leaderboard.yml with GH_TOKEN, REPO (owner/name) and
# VERIFY (the verify-score binary) set, from the repository's root on main.
#
# One issue that fails (the API, a push) is logged and left open for the next run; the rest
# are still checked, and the script exits 1 at the end. Every step can run again: a run already
# recorded is not counted twice (verify-score sees it in the history) and an issue that has
# the verdict comment does not get a second one.
set -uo pipefail
: "${REPO:?owner/name}" "${VERIFY:?the verify-score binary}"

work="${RUNNER_TEMP:-$(mktemp -d)}/leaderboard"
mkdir -p "$work"

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"

# the labels the form and this script use (the form's must exist before the first submission);
# existing ones are left as they are
gh label create score --repo "$REPO" --color 1d76db --description "Leaderboard submission" >/dev/null 2>&1
gh label create score-accepted --repo "$REPO" --color 0e8a16 --description "Verified by the leaderboard workflow" >/dev/null 2>&1
gh label create score-rejected --repo "$REPO" --color d93f0b --description "Not ranked by the leaderboard workflow" >/dev/null 2>&1

# git with the token as an http header from the environment (the checkout keeps none)
git_auth() {
    if [[ -n ${GH_TOKEN:-} ]]; then
        GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0="http.https://github.com/.extraheader" \
            GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$GH_TOKEN" | base64 | tr -d '\n')" git "$@"
    else
        git "$@"
    fi
}

# pushes HEAD to main; if main moved meanwhile, rebases onto it and tries again
push_main() {
    local attempt
    for attempt in 1 2 3; do
        git_auth push -q origin HEAD:main && return 0
        git_auth pull -q --rebase origin main || { git rebase --abort; return 1; }
    done
    git_auth push -q origin HEAD:main
}

# the verdict comments on the issue already (the marker verify-score ends each with)
VERDICTS='[.[] | select(.user.login == "github-actions[bot]" and (.body | contains("<!-- leaderboard-verdict -->")))] | length'

# comments the verifier's text on issue $1 (unless it has it), labels it $2 and closes it as $3
answer() {
    local answered
    answered=$(gh api "repos/$REPO/issues/$1/comments" --paginate --jq "$VERDICTS" | awk '{ n += $1 } END { print n + 0 }') || return 1
    if [[ $answered == 0 ]]; then
        gh issue comment "$1" --repo "$REPO" --body-file "$work/out/comment.md" >/dev/null || return 1
    fi
    gh issue edit "$1" --repo "$REPO" --add-label "$2" >/dev/null || return 1
    gh issue close "$1" --repo "$REPO" --reason "$3" >/dev/null
}

# checks issue $1; non-zero when something failed on the way
check() {
    local n=$1 outcome label reason
    gh api "repos/$REPO/issues/$n" >"$work/issue.json" || return 1
    rm -rf "$work/out"
    "$VERIFY" --issue "$work/issue.json" --leaderboard leaderboard/leaderboard.json --history leaderboard/history.json \
        --readme README.md --out "$work/out" || return 1
    outcome=$(cat "$work/out/outcome") || return 1
    echo "#$n: $outcome"
    case $outcome in
        placed | verified) label=score-accepted reason=completed ;;
        rejected) label=score-rejected reason="not planned" ;;
        ignored) return 0 ;;
        *)
            echo "unknown outcome for #$n" >&2
            return 1
            ;;
    esac
    if [[ -f $work/out/commit ]]; then
        git add leaderboard/leaderboard.json leaderboard/history.json README.md || return 1
        git commit -q -F "$work/out/commit" || return 1
        push_main || return 1
        # on main now: a failure from here on keeps it
        base=$(git rev-parse HEAD)
    fi
    answer "$n" "$label" "$reason"
}

failed=0
# oldest first: of a run code filed twice, the first issue gets the board
numbers=$(gh issue list --repo "$REPO" --label score --state open --search "sort:created-asc" --limit 100 --json number --jq '.[].number' |
    sort -n) || exit 1
for n in $numbers; do
    [[ $n =~ ^[0-9]+$ ]] || continue
    base=$(git rev-parse HEAD)
    if ! check "$n"; then
        echo "::warning::#$n could not be handled; the next run tries again"
        failed=1
        # nothing half done is carried over to the next issue
        git reset -q --hard "$base"
    fi
done
exit "$failed"
