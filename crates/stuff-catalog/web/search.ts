import MiniSearch, { type Options, type SearchOptions } from "./minisearch/index.ts";

export interface PackageDocument {
  id: number;
  name: string;
  registry: string;
  source: string;
}

export function foldTerm(term: string): string {
  return term.normalize("NFKD").replace(/\p{M}/gu, "").toLocaleLowerCase("und");
}

const INDEX_OPTIONS: Options<PackageDocument> = {
  fields: ["name", "registry", "source"],
  processTerm: foldTerm,
};

const SEARCH_OPTIONS: SearchOptions = {
  prefix: true,
  fuzzy: (term) => (term.length >= 4 ? 0.2 : false),
  combineWith: "AND",
  boost: { name: 4 },
};

export type PackageSearch = (query: string) => number[] | null;

/** Ids of matching documents, best first, or null when the query has nothing to match. */
export function createPackageSearch(documents: readonly PackageDocument[]): PackageSearch {
  const index = new MiniSearch<PackageDocument>(INDEX_OPTIONS);
  index.addAll(documents);
  return (query) => {
    const trimmed = query.trim();
    if (!trimmed) return null;
    return index.search(trimmed, SEARCH_OPTIONS).map((result) => Number(result.id));
  };
}
