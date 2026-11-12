from __future__ import annotations

RELEASE_NOTES_STEP_NAME = "Write release notes"
RELEASE_NOTES_FLAG = '--notes-file "$RUNNER_TEMP/release-notes.md"'

SCRIPT = r"""notes="$RUNNER_TEMP/release-notes.md"
previous=$(gh release list --repo "$GITHUB_REPOSITORY" --exclude-drafts --limit 1 \
  --json tagName --jq '.[0].tagName // ""')
if [[ -z "$previous" ]]; then
  printf 'Initial release at %s.\n' "$GITHUB_SHA" > "$notes"
  exit 0
fi
compare="repos/$GITHUB_REPOSITORY/compare/$previous...$GITHUB_SHA"
status=$(gh api "$compare" --jq '.status')
if [[ "$status" != ahead ]]; then
  printf 'Release notes rejected: %s is %s relative to %s, expected ahead\n' "$GITHUB_SHA" "$status" "$previous" >&2
  exit 1
fi
{
  printf '## Changes since %s\n\n' "$previous"
  gh api --paginate "$compare?per_page=100" --jq '.commits[] | "- \(.sha[0:12]) \(.commit.message | split("\n")[0])"'
  printf '\n**Full changelog**: %s/%s/compare/%s...%s\n' \
    "$GITHUB_SERVER_URL" "$GITHUB_REPOSITORY" "$previous" "$GITHUB_SHA"
} > "$notes"
"""


def render_workflow_step(indentation: int) -> list[str]:
    prefix = " " * indentation
    child = prefix + "  "
    script = child + "  "
    return [
        f"{prefix}- name: {RELEASE_NOTES_STEP_NAME}",
        f"{child}env:",
        f"{script}GH_TOKEN: ${{{{ github.token }}}}",
        f"{child}run: |",
        *(f"{script}{line}" for line in SCRIPT.splitlines()),
    ]
