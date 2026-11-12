interface SearchEntry {
  readonly purl: string;
  readonly name: string;
  readonly ecosystem: string;
  readonly label: string;
}

interface Window {
  DEPS_INDEX?: SearchEntry[];
  searchDeps?: (query: string) => SearchEntry[];
}

declare const __DEPS_INDEX__: SearchEntry[];
