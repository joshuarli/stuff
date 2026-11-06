(() => {
  "use strict";

  function recordValue(value: unknown): Record<string, unknown> | undefined {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
    return value as Record<string, unknown>;
  }

  function stringList(value: unknown): string[] | undefined {
    if (!Array.isArray(value)) return undefined;
    const items: string[] = [];
    for (const item of value) {
      if (typeof item === "string") items.push(item);
    }
    return items;
  }

  function decodeLandscape(record: Record<string, unknown>): PageData {
    const membership: Record<string, LandscapeMembership> = {};
    const rawMembership = recordValue(record["membership"]);
    if (rawMembership !== undefined) {
      for (const [purl, raw] of Object.entries(rawMembership)) {
        const entry = recordValue(raw);
        if (entry === undefined) continue;
        const direct = stringList(entry["direct"]);
        const transitive = stringList(entry["transitive"]);
        membership[purl] = {
          ...(direct === undefined ? {} : { direct }),
          ...(transitive === undefined ? {} : { transitive }),
        };
      }
    }
    const labels: Record<string, string> = {};
    const rawLabels = recordValue(record["labels"]);
    if (rawLabels !== undefined) {
      for (const [purl, label] of Object.entries(rawLabels)) {
        if (typeof label === "string") labels[purl] = label;
      }
    }
    return { membership, labels };
  }

  function pageData(): PageData {
    const node = document.getElementById("page-data");
    if (node === null) return {};
    try {
      const parsed: unknown = JSON.parse(node.textContent ?? "{}");
      const record = recordValue(parsed);
      return record === undefined ? {} : decodeLandscape(record);
    } catch {
      return {};
    }
  }

  const data = pageData();
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
      const rows = Array.from(document.querySelectorAll<HTMLElement>("tr[data-row], .row[data-row]"));
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
      if (page === "landscape") {
        const add = document.createElement("button");
        add.type = "button";
        add.textContent = "+ column";
        add.setAttribute("data-add-column", entry.purl);
        row.appendChild(add);
      }
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

  function sortRows(key: string, direction: string): void {
    const table = document.querySelector<HTMLTableElement>("table.landscape");
    if (table === null) return;
    const body = table.tBodies[0];
    if (body === undefined) return;
    const rows = Array.from(body.rows);
    rows.sort((left, right) => {
      const leftValue = left.getAttribute("data-" + key) ?? "";
      const rightValue = right.getAttribute("data-" + key) ?? "";
      let comparison: number;
      if (key === "commonality" || key === "coverage") {
        comparison = Number(leftValue) - Number(rightValue);
      } else if (leftValue < rightValue) {
        comparison = -1;
      } else if (leftValue > rightValue) {
        comparison = 1;
      } else {
        comparison = 0;
      }
      if (comparison !== 0) return direction === "asc" ? comparison : -comparison;
      return (left.getAttribute("data-repo") ?? "").localeCompare(right.getAttribute("data-repo") ?? "");
    });
    for (const row of rows) body.appendChild(row);
  }

  document.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const button = target.closest<HTMLElement>("[data-sort]");
    if (button === null) return;
    const key = button.getAttribute("data-sort");
    if (key === null) return;
    const direction = button.getAttribute("data-direction") === "asc" ? "desc" : "asc";
    for (const item of document.querySelectorAll<HTMLElement>("[data-sort]")) {
      item.removeAttribute("data-direction");
    }
    button.setAttribute("data-direction", direction);
    sortRows(key, direction);
  });

  window.addLandscapeColumn = (purl: string): void => {
    const table = document.querySelector<HTMLTableElement>("table.landscape");
    const membershipMap = data.membership;
    if (table === null || membershipMap === undefined) return;
    const head = table.tHead;
    if (head === null) return;
    const header = head.rows[0];
    if (header === undefined) return;
    const body = table.tBodies[0];
    if (body === undefined) return;
    const label = data.labels?.[purl] ?? purl;
    const th = document.createElement("th");
    th.className = "rotate";
    th.textContent = label;
    header.appendChild(th);
    const membership = membershipMap[purl];
    for (const row of Array.from(body.rows)) {
      const repo = row.getAttribute("data-repo");
      if (repo === null) continue;
      const cell = document.createElement("td");
      cell.className = "cell";
      if (membership?.direct?.includes(repo) === true) {
        cell.className = "cell direct";
        cell.title = "direct";
      } else if (membership?.transitive?.includes(repo) === true) {
        cell.className = "cell transitive";
        cell.title = "transitive";
      }
      row.appendChild(cell);
    }
  };

  document.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const button = target.closest<HTMLElement>("[data-add-column]");
    if (button === null) return;
    const purl = button.getAttribute("data-add-column");
    if (purl === null) return;
    const add = window.addLandscapeColumn;
    if (typeof add === "function") add(purl);
  });
})();
