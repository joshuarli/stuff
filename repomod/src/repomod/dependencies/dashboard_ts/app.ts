(() => {
  "use strict";

  const page = document.body.getAttribute("data-page") ?? "";
  const prefix = document.body.getAttribute("data-prefix") ?? "";

  document.addEventListener("keydown", (event) => {
    const target = event.target;
    const typing =
      target instanceof HTMLElement &&
      (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
    if (event.key === "/" && !typing) {
      const search = document.getElementById("search");
      if (search !== null) {
        event.preventDefault();
        search.focus();
      }
      return;
    }
    if (typing) return;
    if (event.key === "j" || event.key === "k") {
      const rows = Array.from(document.querySelectorAll<HTMLElement>("tr[data-row]")).filter(
        (row) => row.offsetParent !== null,
      );
      if (rows.length === 0) return;
      event.preventDefault();
      const index = rows.findIndex((row) => row.classList.contains("selected"));
      const next = index < 0 ? 0 : event.key === "j" ? Math.min(rows.length - 1, index + 1) : Math.max(0, index - 1);
      for (const row of rows) row.classList.remove("selected");
      const selected = rows[next];
      if (selected === undefined) return;
      selected.classList.add("selected");
      selected.scrollIntoView({ block: "nearest" });
    }
    if (event.key === "Enter" && !typing) {
      const selected = document.querySelector<HTMLElement>(".selected[data-href]");
      if (selected !== null) {
        const href = selected.getAttribute("data-href");
        if (href !== null) window.location.href = href;
      }
    }
  });

  document.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const button = target.closest<HTMLElement>("[data-copy]");
    if (button === null) return;
    const value = button.getAttribute("data-copy") ?? "";
    const clipboard: Clipboard | undefined = navigator.clipboard;
    if (clipboard === undefined) return;
    void clipboard.writeText(value).then(
      () => {
        button.setAttribute("data-copied", "1");
        button.textContent = "copied";
        window.setTimeout(() => {
          button.removeAttribute("data-copied");
          button.textContent = "copy";
        }, 1200);
      },
      () => undefined,
    );
  });

  function setupTable(scope: HTMLElement): void {
    const rows = Array.from(scope.querySelectorAll<HTMLTableRowElement>("tbody tr"));
    const chips = Array.from(scope.querySelectorAll<HTMLButtonElement>("[data-filter]"));
    const search = scope.querySelector<HTMLInputElement>("[data-table-search]");
    const more = scope.querySelector<HTMLButtonElement>("[data-more]");
    const empty = scope.querySelector<HTMLElement>("[data-empty]");
    const limit = Number(scope.dataset["limit"] ?? "0");
    let expanded = false;

    function matches(row: HTMLTableRowElement): boolean {
      const pressed = chips.find((chip) => chip.getAttribute("aria-pressed") === "true");
      const tag = pressed?.dataset["filter"] ?? "";
      const tags = (row.dataset["tags"] ?? "").split(" ");
      const text = (search?.value ?? "").trim().toLowerCase();
      return (tag === "" || tags.includes(tag)) && (text === "" || (row.textContent ?? "").toLowerCase().includes(text));
    }

    function update(): void {
      const matching = rows.filter(matches);
      const visible = new Set(limit > 0 && !expanded ? matching.slice(0, limit) : matching);
      for (const row of rows) row.hidden = !visible.has(row);
      if (empty !== null) empty.hidden = matching.length > 0;
      if (more !== null) {
        const hidden = matching.length - visible.size;
        more.hidden = hidden === 0;
        more.textContent = "Show " + hidden + " more";
      }
    }

    for (const chip of chips) {
      chip.addEventListener("click", () => {
        for (const other of chips) other.setAttribute("aria-pressed", other === chip ? "true" : "false");
        expanded = false;
        update();
      });
    }
    search?.addEventListener("input", () => {
      expanded = false;
      update();
    });
    more?.addEventListener("click", () => {
      expanded = true;
      update();
    });
    update();
  }

  for (const scope of document.querySelectorAll<HTMLElement>("[data-table-scope]")) setupTable(scope);

  const input = document.querySelector<HTMLInputElement>("#search");
  const results = document.querySelector<HTMLElement>("#search-results");

  function closeResults(): void {
    if (results === null) return;
    results.innerHTML = "";
    results.style.display = "none";
  }

  function renderResults(): void {
    if (input === null || results === null) return;
    const search = window.searchDeps;
    if (typeof search !== "function") return;
    const found = search(input.value);
    results.innerHTML = "";
    if (found.length === 0) {
      closeResults();
      return;
    }
    for (const entry of found) {
      const row = document.createElement("div");
      const link = document.createElement("a");
      link.href = prefix + "packages.html#pkg=" + encodeURIComponent(entry.purl);
      link.textContent = entry.label + " @ " + entry.ecosystem;
      const meta = document.createElement("span");
      meta.className = "meta";
      meta.textContent = "  " + entry.purl;
      link.appendChild(meta);
      row.appendChild(link);
      results.appendChild(row);
    }
    results.style.display = "block";
  }

  if (input !== null) {
    input.addEventListener("input", renderResults);
    input.addEventListener("focus", renderResults);
    document.addEventListener("click", (event) => {
      const target = event.target;
      if (target === input) return;
      if (results !== null && target instanceof Node && results.contains(target)) return;
      closeResults();
    });
  }

  function routePackage(): void {
    if (page !== "packages") return;
    const sections = Array.from(document.querySelectorAll<HTMLElement>(".package"));
    const list = document.getElementById("package-index");
    let match: HTMLElement | null = null;
    const hash = window.location.hash || "";
    let wanted: string | null = null;
    if (hash.startsWith("#pkg=")) {
      try {
        wanted = decodeURIComponent(hash.slice(5));
      } catch {
        wanted = null;
      }
    }
    for (const section of sections) {
      const active = wanted !== null && section.getAttribute("data-purl") === wanted;
      section.classList.toggle("active", active);
      if (active) match = section;
    }
    if (list !== null) list.style.display = match !== null ? "none" : "block";
    if (match !== null) match.scrollIntoView({ block: "start" });
  }

  window.addEventListener("hashchange", routePackage);
  routePackage();
})();
