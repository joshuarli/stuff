interface SearchEntry {
  readonly purl: string;
  readonly name: string;
  readonly ecosystem: string;
  readonly label: string;
}

type WeightLang = "npm" | "cargo" | "pypi" | "go" | "other";
type WeightPlatform = "darwin-arm64" | "linux-x64-musl" | "linux-arm64-musl" | "all";

interface WeightTotals {
  bytes: number;
  packages: number;
  unknown: number;
}

interface WeightRepo {
  repo: string;
  totals: Record<string, Record<WeightLang, WeightTotals>>;
}

interface WeightDep {
  id: string;
  lang: WeightLang;
  name: string;
  repo: string;
  frees: Record<string, WeightTotals>;
  shared: Record<string, WeightTotals>;
}

interface WeightView {
  langs: WeightLang[];
  platform: WeightPlatform;
  dev: boolean;
  repo: string | null;
}

interface DepsWeight {
  readonly LANGS: readonly WeightLang[];
  formatBytes(value: number): string;
  parseHash(hash: string): WeightView;
  formatHash(view: WeightView): string;
  summarize(row: WeightRepo, view: WeightView): WeightTotals;
  order(rows: readonly WeightRepo[], view: WeightView): string[];
  valueText(sum: WeightTotals): string;
  noteText(sum: WeightTotals): string;
  tipText(row: WeightRepo, view: WeightView): string;
  emptyText(rows: readonly WeightRepo[], view: WeightView, names: number): string;
  cellText(lang: WeightLang, sum: WeightTotals): [string, string];
  depNote(frees: WeightTotals, shared: WeightTotals): string;
  depTip(label: string, repo: string, frees: WeightTotals, shared: WeightTotals): string;
  depOrder(deps: readonly WeightDep[], view: WeightView): string[];
  depCount(shown: number, total: number): string;
}

interface Window {
  DEPS_INDEX?: SearchEntry[];
  searchDeps?: (query: string) => SearchEntry[];
  depsWeight?: DepsWeight;
}

declare const __DEPS_INDEX__: SearchEntry[];
