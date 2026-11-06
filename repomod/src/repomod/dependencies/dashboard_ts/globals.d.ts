interface SearchEntry {
  readonly purl: string;
  readonly name: string;
  readonly ecosystem: string;
  readonly label: string;
}

interface LandscapeMembership {
  readonly direct?: readonly string[];
  readonly transitive?: readonly string[];
}

interface LandscapeData {
  readonly membership?: Readonly<Record<string, LandscapeMembership>>;
  readonly labels?: Readonly<Record<string, string>>;
}

type PageData = LandscapeData;

interface Window {
  DEPS_INDEX?: SearchEntry[];
  searchDeps?: (query: string) => SearchEntry[];
  addLandscapeColumn?: (purl: string) => void;
}

declare const __DEPS_INDEX__: SearchEntry[];
