import { createPackageSearch, visibleMatches } from "./search.ts";

const FIRST_PAGE = 20;

function enhance(): void {
  const box = document.querySelector<HTMLElement>("[data-package-search-box]");
  const input = document.querySelector<HTMLInputElement>("[data-package-search]");
  const list = document.querySelector<HTMLOListElement>("[data-packages]");
  const browse = document.querySelector<HTMLDetailsElement>("[data-browse]");
  const empty = document.querySelector<HTMLElement>("[data-no-match]");
  const more = document.querySelector<HTMLButtonElement>("[data-more]");
  if (!box || !input || !list || !browse || !empty || !more) return;

  const rows = [...list.children] as HTMLElement[];
  const search = createPackageSearch(
    rows.map((row, id) => ({
      id,
      name: row.dataset["name"] ?? "",
      registry: row.dataset["registry"] ?? "",
      source: row.dataset["source"] ?? "",
    })),
  );
  let limit = FIRST_PAGE;

  const apply = (): void => {
    for (const row of rows) row.hidden = true;
    const ids = search(input.value);
    if (ids === null) {
      empty.hidden = true;
      more.hidden = true;
      return;
    }
    const matched = ids.flatMap((id) => rows[id] ?? []);
    const { shown, hidden } = visibleMatches(matched.length, limit);
    const visible = matched.slice(0, shown);
    for (const row of visible) row.hidden = false;
    list.append(...visible);
    empty.hidden = matched.length > 0;
    more.hidden = hidden === 0;
    more.textContent = `Show ${hidden.toLocaleString("en-US")} more`;
  };

  box.hidden = false;
  browse.open = true;
  browse.classList.add("enhanced");
  input.addEventListener("input", () => {
    limit = FIRST_PAGE;
    apply();
  });
  more.addEventListener("click", () => {
    limit = Number.POSITIVE_INFINITY;
    apply();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select, [contenteditable]")) return;
    event.preventDefault();
    if (location.hash && location.hash !== "#find") location.hash = "find";
    input.focus();
  });
  apply();
}

enhance();
