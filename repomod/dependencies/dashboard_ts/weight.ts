(() => {
  "use strict";

  const LANGS: readonly WeightLang[] = ["npm", "cargo", "pypi", "go", "other"];
  const SHORT: Record<WeightLang, string> = {
    npm: "TS/JS",
    cargo: "Rust",
    pypi: "Python",
    go: "Go",
    other: "Other",
  };
  const UNITS = ["B", "kB", "MB", "GB", "TB"];

  function formatBytes(value: number): string {
    if (value < 1000) return value + " B";
    let amount = value;
    let index = 0;
    while (amount >= 1000 && index < UNITS.length - 1) {
      amount /= 1000;
      index += 1;
    }
    let rounded = amount < 10 ? Math.floor(amount * 10 + 0.5) / 10 : Math.floor(amount + 0.5);
    if (rounded >= 1000 && index < UNITS.length - 1) {
      rounded = Math.floor((rounded / 1000) * 10 + 0.5) / 10;
      index += 1;
    }
    const unit = UNITS[index] ?? "TB";
    return (rounded < 10 ? rounded.toFixed(1) : rounded.toFixed(0)) + " " + unit;
  }

  const PLATFORMS: readonly WeightPlatform[] = ["darwin-arm64", "linux-x64-musl", "linux-arm64-musl", "all"];
  const DEFAULT_PLATFORM: WeightPlatform = "darwin-arm64";
  const SIZED: readonly WeightLang[] = ["npm", "cargo", "pypi"];

  function isLang(value: string): value is WeightLang {
    return (LANGS as readonly string[]).includes(value);
  }

  function isPlatform(value: string): value is WeightPlatform {
    return (PLATFORMS as readonly string[]).includes(value);
  }

  function variantKey(view: WeightView): string {
    return view.platform + (view.dev ? "-dev" : "-run");
  }

  function parseHash(hash: string): WeightView {
    const params = new URLSearchParams(hash.replace(/^#/, ""));
    const wanted = (params.get("lang") ?? "").split(",").filter(isLang);
    const langs = LANGS.filter((lang) => wanted.includes(lang));
    const platform = params.get("platform") ?? "";
    return {
      langs: langs.length > 0 ? langs : [...LANGS],
      platform: isPlatform(platform) ? platform : DEFAULT_PLATFORM,
      dev: params.get("dev") === "1",
      repo: params.get("repo"),
    };
  }

  function formatHash(view: WeightView): string {
    const parts: string[] = [];
    if (view.langs.length !== LANGS.length) parts.push("lang=" + view.langs.join(","));
    if (view.platform !== DEFAULT_PLATFORM) parts.push("platform=" + view.platform);
    if (view.dev) parts.push("dev=1");
    if (view.repo !== null) parts.push("repo=" + encodeURIComponent(view.repo).replace(/%2F/g, "/"));
    return parts.join("&");
  }

  function summarize(row: WeightRepo, view: WeightView): WeightTotals {
    const sum: WeightTotals = { bytes: 0, packages: 0, unknown: 0 };
    const variant = row.totals[variantKey(view)];
    if (variant === undefined) return sum;
    for (const lang of view.langs) {
      const part = variant[lang];
      sum.bytes += part.bytes;
      sum.packages += part.packages;
      sum.unknown += part.unknown;
    }
    return sum;
  }

  function order(rows: readonly WeightRepo[], view: WeightView): string[] {
    return rows
      .map((row) => ({ repo: row.repo, sum: summarize(row, view) }))
      .sort((left, right) => {
        if (left.sum.bytes !== right.sum.bytes) return right.sum.bytes - left.sum.bytes;
        if (left.sum.packages !== right.sum.packages) return right.sum.packages - left.sum.packages;
        const a = left.repo.toLowerCase();
        const b = right.repo.toLowerCase();
        return a < b ? -1 : a > b ? 1 : 0;
      })
      .map((entry) => entry.repo);
  }

  function valueText(sum: WeightTotals): string {
    if (sum.bytes > 0) return formatBytes(sum.bytes);
    return sum.packages > 0 ? "size unknown" : "none";
  }

  function noteText(sum: WeightTotals): string {
    if (sum.packages === 0) return "no packages";
    const count = sum.packages + (sum.packages === 1 ? " package" : " packages");
    return sum.unknown > 0 ? count + " · " + sum.unknown + " size unknown" : count;
  }

  function tipText(row: WeightRepo, view: WeightView): string {
    const sum = summarize(row, view);
    const variant = row.totals[variantKey(view)];
    const split = view.langs
      .filter((lang) => (variant?.[lang].bytes ?? 0) > 0)
      .map((lang) => SHORT[lang] + " " + formatBytes(variant?.[lang].bytes ?? 0));
    const detail = split.length > 1 ? " (" + split.join(", ") + ")" : "";
    return row.repo + ": " + valueText(sum) + detail + ", " + noteText(sum);
  }

  function emptyText(rows: readonly WeightRepo[], view: WeightView, names: number): string {
    const empty = rows
      .filter((row) => summarize(row, view).packages === 0)
      .map((row) => row.repo)
      .sort((left, right) => {
        const a = left.toLowerCase();
        const b = right.toLowerCase();
        return a < b ? -1 : a > b ? 1 : 0;
      });
    if (empty.length === 0) return "";
    const what = view.langs.length === LANGS.length ? "third-party" : view.langs.map((lang) => SHORT[lang]).join(" or ");
    const count = empty.length + (empty.length === 1 ? " repository has" : " repositories have") + " no " + what + " dependencies";
    const more = empty.length > names ? " and " + (empty.length - names) + " more" : "";
    return count + ": " + empty.slice(0, names).join(", ") + more + ".";
  }

  function cellText(lang: WeightLang, sum: WeightTotals): [string, string] {
    if (sum.packages === 0) return ["\u2014", ""];
    if (SIZED.includes(lang) && sum.bytes > 0) return [formatBytes(sum.bytes), String(sum.packages)];
    return [String(sum.packages), "no size"];
  }

  const ZERO: WeightTotals = { bytes: 0, packages: 0, unknown: 0 };

  function plural(count: number, word: string): string {
    return count + " " + word + (count === 1 ? "" : "s");
  }

  function depNote(frees: WeightTotals, shared: WeightTotals): string {
    const count = plural(frees.packages, "package") + " only via it";
    if (shared.packages === 0) return count;
    const amount = shared.bytes > 0 ? formatBytes(shared.bytes) : plural(shared.packages, "package");
    return count + " · " + amount + " shared";
  }

  function depTip(label: string, repo: string, frees: WeightTotals, shared: WeightTotals): string {
    return label + " in " + repo + ": removing it frees " + valueText(frees) + ", " + depNote(frees, shared);
  }

  function compareText(left: string, right: string): number {
    const a = left.toLowerCase();
    const b = right.toLowerCase();
    return a < b ? -1 : a > b ? 1 : 0;
  }

  function depOrder(deps: readonly WeightDep[], view: WeightView): string[] {
    const key = variantKey(view);
    return deps
      .filter((dep) => view.langs.includes(dep.lang) && (dep.frees[key] ?? ZERO).packages > 0)
      .map((dep) => ({ dep, frees: dep.frees[key] ?? ZERO, shared: dep.shared[key] ?? ZERO }))
      .sort((left, right) => {
        if (left.frees.bytes !== right.frees.bytes) return right.frees.bytes - left.frees.bytes;
        if (left.frees.packages !== right.frees.packages) return right.frees.packages - left.frees.packages;
        if (left.shared.bytes !== right.shared.bytes) return right.shared.bytes - left.shared.bytes;
        return compareText(left.dep.name, right.dep.name) || compareText(left.dep.repo, right.dep.repo);
      })
      .map((entry) => entry.dep.id);
  }

  function depCount(shown: number, total: number): string {
    if (total === 0) return "No direct dependency in the selection brings in third-party packages.";
    return shown < total ? "Top " + shown + " of " + total + " direct dependencies." : total + " direct dependencies.";
  }

  const handle: DepsWeight = {
    LANGS,
    formatBytes,
    parseHash,
    formatHash,
    summarize,
    order,
    valueText,
    noteText,
    tipText,
    emptyText,
    cellText,
    depNote,
    depTip,
    depOrder,
    depCount,
  };
  window.depsWeight = handle;

  const found = document.querySelector<HTMLElement>("[data-weight]");
  if (found === null) return;
  const root: HTMLElement = found;

  function number(element: Element, name: string): number {
    const value = Number(element.getAttribute(name));
    return Number.isFinite(value) ? value : 0;
  }

  function readRow(element: HTMLElement): WeightRepo {
    const totals: Record<string, Record<WeightLang, WeightTotals>> = {};
    for (const platform of PLATFORMS) {
      for (const scope of ["run", "dev"]) {
        const variant = {} as Record<WeightLang, WeightTotals>;
        for (const lang of LANGS) {
          const [bytes, packages, unknown] = (element.getAttribute("data-t-" + platform + "-" + scope + "-" + lang) ?? "0,0,0")
            .split(",")
            .map(Number);
          variant[lang] = { bytes: bytes ?? 0, packages: packages ?? 0, unknown: unknown ?? 0 };
        }
        totals[platform + "-" + scope] = variant;
      }
    }
    return { repo: element.getAttribute("data-repo") ?? "", totals };
  }

  const list = root.querySelector<HTMLElement>("[data-weight-list]");
  const emptyLine = root.querySelector<HTMLElement>("[data-empty-line]");
  const rowElements = new Map<string, HTMLElement>();
  const rows: WeightRepo[] = [];
  for (const element of root.querySelectorAll<HTMLElement>("[data-weight-row]")) {
    const row = readRow(element);
    rows.push(row);
    rowElements.set(row.repo, element);
  }
  const tableBody = root.querySelector<HTMLElement>("[data-weight-table]");
  const tableRows = new Map<string, HTMLElement>();
  for (const element of root.querySelectorAll<HTMLElement>("[data-weight-table-row]")) {
    tableRows.set(element.getAttribute("data-repo") ?? "", element);
  }
  function totals(element: Element, prefix: string): Record<string, WeightTotals> {
    const found: Record<string, WeightTotals> = {};
    for (const platform of PLATFORMS) {
      for (const scope of ["run", "dev"]) {
        const [bytes, packages, unknown] = (element.getAttribute(prefix + platform + "-" + scope) ?? "0,0,0")
          .split(",")
          .map(Number);
        found[platform + "-" + scope] = { bytes: bytes ?? 0, packages: packages ?? 0, unknown: unknown ?? 0 };
      }
    }
    return found;
  }

  function readDeps(container: Element | null): Map<string, [WeightDep, HTMLElement]> {
    const found = new Map<string, [WeightDep, HTMLElement]>();
    if (container === null) return found;
    for (const element of container.querySelectorAll<HTMLElement>("[data-dep]")) {
      const lang = element.getAttribute("data-lang") ?? "";
      if (!isLang(lang)) continue;
      const id = String(found.size);
      const dep = {
        id,
        lang,
        name: element.getAttribute("data-name") ?? "",
        repo: element.getAttribute("data-repo") ?? "",
        frees: totals(element, "data-f-"),
        shared: totals(element, "data-s-"),
      };
      found.set(id, [dep, element]);
    }
    return found;
  }

  const depList = root.querySelector<HTMLElement>("[data-dep-list]");
  const depCaption = root.querySelector<HTMLElement>("[data-dep-count]");
  const depLimit = number(root, "data-dep-limit");
  const fleetDeps = readDeps(depList);
  const repoDeps = Array.from(root.querySelectorAll<HTMLElement>("[data-deps]")).map((body) => ({ body, deps: readDeps(body) }));
  const langToggles = Array.from(root.querySelectorAll<HTMLElement>("[data-lang-toggle]"));
  const platformToggles = Array.from(root.querySelectorAll<HTMLElement>("[data-platform-toggle]"));
  const devToggle = root.querySelector<HTMLElement>("[data-dev-toggle]");
  const limit = number(root, "data-limit");
  const names = number(root, "data-names");
  let view = parseHash(window.location.hash);

  function renderRow(element: HTMLElement, row: WeightRepo, scale: number): void {
    const sum = summarize(row, view);
    element.hidden = sum.packages === 0;
    const summary = element.querySelector<HTMLElement>("[data-summary]");
    summary?.setAttribute("data-tip", tipText(row, view));
    const fill = element.querySelector<HTMLElement>("[data-fill]");
    if (fill !== null) {
      fill.style.setProperty("--r", (scale > 0 ? sum.bytes / scale : 0).toFixed(4));
      fill.hidden = sum.bytes === 0;
    }
    const value = element.querySelector<HTMLElement>("[data-value]");
    if (value !== null) {
      value.textContent = valueText(sum);
      value.classList.toggle("weight-none", sum.bytes === 0);
    }
    const note = element.querySelector<HTMLElement>("[data-note]");
    if (note !== null) note.textContent = noteText(sum);
    const body = element.querySelector<HTMLElement>("[data-packages]");
    if (body === null) return;
    const items = Array.from(body.querySelectorAll<HTMLElement>("tr[data-lang]"));
    const shown = items
      .filter(
        (item) =>
          view.langs.some((lang) => lang === item.getAttribute("data-lang")) &&
          (item.getAttribute("data-platforms") ?? "").split(" ").includes(view.platform) &&
          (view.dev || item.getAttribute("data-dev") !== "1"),
      )
      .sort((left, right) => number(right, "data-bytes") - number(left, "data-bytes"));
    const visible = new Set(shown.slice(0, limit > 0 ? limit : shown.length));
    for (const item of shown) body.appendChild(item);
    for (const item of items) item.hidden = !visible.has(item);
    const caption = element.querySelector<HTMLElement>("[data-count]");
    if (caption !== null) {
      caption.textContent =
        visible.size === 0
          ? "The selected languages report no package sizes."
          : "Heaviest " + visible.size + " of " + sum.packages + " packages" + (sum.unknown > 0 ? ", " + sum.unknown + " without a size" : "") + ".";
    }
  }

  function renderTable(element: HTMLElement, row: WeightRepo): void {
    const sum = summarize(row, view);
    const variant = row.totals[variantKey(view)];
    for (const cell of element.querySelectorAll<HTMLElement>("[data-cell]")) {
      const lang = cell.getAttribute("data-cell") ?? "";
      if (!isLang(lang) || variant === undefined) continue;
      const [text, note] = cellText(lang, variant[lang]);
      cell.textContent = text;
      if (note !== "") {
        const span = document.createElement("span");
        span.className = "cell-note";
        span.textContent = note;
        cell.append(" ", span);
      }
    }
    const bytes = element.querySelector<HTMLElement>("[data-total-bytes]");
    if (bytes !== null) bytes.textContent = valueText(sum);
    const packages = element.querySelector<HTMLElement>("[data-total-packages]");
    if (packages !== null) packages.textContent = String(sum.packages);
    const unknown = element.querySelector<HTMLElement>("[data-total-unknown]");
    if (unknown !== null) unknown.textContent = String(sum.unknown);
  }

  function renderDeps(): void {
    const key = variantKey(view);
    const sorted = depOrder(Array.from(fleetDeps.values(), ([dep]) => dep), view);
    const visible = new Set(sorted.slice(0, depLimit > 0 ? depLimit : sorted.length));
    const scale = Math.max(
      0,
      ...Array.from(visible, (id) => {
        const dep = fleetDeps.get(id)?.[0];
        return dep === undefined ? 0 : (dep.frees[key] ?? ZERO).bytes + (dep.shared[key] ?? ZERO).bytes;
      }),
    );
    for (const id of sorted) {
      const entry = fleetDeps.get(id);
      if (entry !== undefined) depList?.appendChild(entry[1]);
    }
    for (const [id, [dep, element]] of fleetDeps) {
      element.hidden = !visible.has(id);
      if (element.hidden) continue;
      const frees = dep.frees[key] ?? ZERO;
      const shared = dep.shared[key] ?? ZERO;
      const drawn = frees.bytes + shared.bytes;
      element.querySelector("[data-tip]")?.setAttribute(
        "data-tip",
        depTip(element.getAttribute("data-label") ?? "", element.getAttribute("data-repo") ?? "", frees, shared),
      );
      const fill = element.querySelector<HTMLElement>("[data-fill]");
      if (fill !== null) {
        fill.style.setProperty("--r", (scale > 0 ? drawn / scale : 0).toFixed(4));
        fill.hidden = drawn === 0;
      }
      for (const [selector, value] of [["[data-frees]", frees.bytes], ["[data-shared]", shared.bytes]] as const) {
        const segment = element.querySelector<HTMLElement>(selector);
        if (segment === null) continue;
        segment.style.flexGrow = String(value);
        segment.hidden = value === 0;
      }
      const value = element.querySelector<HTMLElement>("[data-value]");
      if (value !== null) {
        value.textContent = valueText(frees);
        value.classList.toggle("weight-none", frees.bytes === 0);
      }
      const note = element.querySelector<HTMLElement>("[data-dep-note]");
      if (note !== null) note.textContent = depNote(frees, shared);
    }
    if (depCaption !== null) depCaption.textContent = depCount(visible.size, sorted.length);
    for (const { body, deps } of repoDeps) {
      const order = depOrder(Array.from(deps.values(), ([dep]) => dep), view);
      const shown = new Set(order.slice(0, limit > 0 ? limit : order.length));
      for (const id of order) {
        const entry = deps.get(id);
        if (entry !== undefined) body.appendChild(entry[1]);
      }
      for (const [id, [dep, element]] of deps) {
        element.hidden = !shown.has(id);
        const frees = dep.frees[key] ?? ZERO;
        const text = element.querySelector<HTMLElement>("[data-frees-text]");
        if (text !== null) text.textContent = valueText(frees);
        const note = element.querySelector<HTMLElement>("[data-dep-note]");
        if (note !== null) note.textContent = depNote(frees, dep.shared[key] ?? ZERO);
      }
    }
  }

  function render(): void {
    const selected = view.langs;
    for (const toggle of langToggles) {
      const lang = toggle.getAttribute("data-lang-toggle") ?? "";
      toggle.setAttribute("aria-pressed", String(selected.some((item) => item === lang)));
    }
    for (const toggle of platformToggles) {
      toggle.setAttribute("aria-pressed", String(toggle.getAttribute("data-platform-toggle") === view.platform));
    }
    devToggle?.setAttribute("aria-pressed", String(view.dev));
    for (const element of root.querySelectorAll<HTMLElement>("[data-lang-show]")) {
      const lang = element.getAttribute("data-lang-show") ?? "";
      element.hidden = !selected.some((item) => item === lang);
    }
    const scale = Math.max(0, ...rows.map((row) => summarize(row, view).bytes));
    const sorted = order(rows, view);
    for (const row of rows) {
      const element = rowElements.get(row.repo);
      if (element !== undefined) renderRow(element, row, scale);
      const tableRow = tableRows.get(row.repo);
      if (tableRow !== undefined) renderTable(tableRow, row);
    }
    for (const repo of sorted) {
      const element = rowElements.get(repo);
      if (element !== undefined) list?.appendChild(element);
      const tableRow = tableRows.get(repo);
      if (tableRow !== undefined) tableBody?.appendChild(tableRow);
    }
    renderDeps();
    if (emptyLine !== null) {
      const text = emptyText(rows, view, names);
      emptyLine.textContent = text;
      emptyLine.hidden = text === "";
    }
  }

  function reveal(): void {
    for (const [repo, element] of rowElements) element.classList.toggle("selected", repo === view.repo);
    if (view.repo === null) return;
    const target = rowElements.get(view.repo);
    if (target === undefined) return;
    const details = target.querySelector<HTMLDetailsElement>("details");
    if (details !== null) details.open = true;
    target.scrollIntoView({ block: "start" });
  }

  function change(next: Partial<WeightView>): void {
    view = { ...view, ...next };
    const hash = formatHash(view);
    window.history.replaceState(null, "", hash === "" ? window.location.pathname + window.location.search : "#" + hash);
    render();
  }

  for (const toggle of langToggles) {
    toggle.addEventListener("click", () => {
      const lang = toggle.getAttribute("data-lang-toggle") ?? "";
      if (!isLang(lang)) return;
      const on = view.langs.includes(lang);
      if (on && view.langs.length === 1) return;
      change({ langs: LANGS.filter((item) => (item === lang ? !on : view.langs.includes(item))) });
    });
  }
  for (const toggle of platformToggles) {
    toggle.addEventListener("click", () => {
      const platform = toggle.getAttribute("data-platform-toggle") ?? "";
      if (isPlatform(platform)) change({ platform });
    });
  }
  devToggle?.addEventListener("click", () => change({ dev: !view.dev }));

  window.addEventListener("hashchange", () => {
    view = parseHash(window.location.hash);
    render();
    reveal();
  });

  const controls = root.querySelector<HTMLElement>("[data-weight-controls]");
  if (controls !== null) controls.hidden = false;
  render();
  reveal();
})();
