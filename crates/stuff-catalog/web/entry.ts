import { createPackageSearch } from "./search.ts";

function enhance(): void {
  const input = document.querySelector<HTMLInputElement>("[data-package-search]");
  const body = document.querySelector<HTMLTableElement>("[data-packages]")?.tBodies[0];
  const empty = document.querySelector<HTMLElement>("[data-no-match]");
  if (!input || !body || !empty) return;

  const rows = [...body.rows];
  const search = createPackageSearch(
    rows.map((row, id) => ({
      id,
      name: row.dataset["name"] ?? "",
      registry: row.dataset["registry"] ?? "",
      source: row.dataset["source"] ?? "",
    })),
  );

  const apply = (): void => {
    const ids = search(input.value);
    if (ids === null) {
      for (const row of rows) row.hidden = false;
      body.append(...rows);
      empty.hidden = true;
      return;
    }
    for (const row of rows) row.hidden = true;
    const matched = ids.flatMap((id) => rows[id] ?? []);
    for (const row of matched) row.hidden = false;
    body.append(...matched);
    empty.hidden = matched.length > 0;
  };

  input.hidden = false;
  input.addEventListener("input", apply);
  if (input.value) apply();
}

enhance();
