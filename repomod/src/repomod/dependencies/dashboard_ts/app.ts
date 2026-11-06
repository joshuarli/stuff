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
