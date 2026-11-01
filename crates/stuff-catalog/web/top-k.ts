/**
 * Keep a sorted, bounded set of scores. Insertion is cheap for small result
 * limits and equal scores retain their input order, matching a stable full sort.
 */
export function selectTop<T>(items: Iterable<T>, limit: number, scoreOf: (item: T) => number): T[] {
  if (limit === 0) return [];
  const selected: { item: T; score: number }[] = [];
  for (const item of items) {
    const score = scoreOf(item);
    const worst = selected[limit - 1];
    if (worst && score <= worst.score) continue;
    let position = selected.length;
    for (let before = selected[position - 1]; before && score > before.score; before = selected[position - 1]) position--;
    selected.splice(position, 0, { item, score });
    if (selected.length > limit) selected.pop();
  }
  return selected.map(({ item }) => item);
}
