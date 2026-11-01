import type { RadixTree, Entry, LeafType } from './types.ts'

// Iterator branches select keys, values, or entries from one shared traversal; these assertions
// specialize its result type to the branch selected at construction.
// oxlint-disable typescript/no-non-null-assertion, typescript/no-unsafe-type-assertion

/** @ignore */
const ENTRIES = 'ENTRIES'

/** @ignore */
const KEYS = 'KEYS'

/** @ignore */
const VALUES = 'VALUES'

/** @ignore */
const LEAF = '' as LeafType

interface Iterators<T> {
  ENTRIES: Entry<T>
  KEYS: string
  VALUES: T
}

type Kind<T> = keyof Iterators<T>
type Result<T, K extends keyof Iterators<T>> = Iterators<T>[K]

type IteratorPath<T> = {
  node: RadixTree<T>,
  keys: string[]
}[]

export type IterableSet<T> = {
  tree: RadixTree<T>,
  prefix: string
}

/**
 * @private
 */
class TreeIterator<T, K extends Kind<T>> implements Iterator<Result<T, K>> {
  set: IterableSet<T>
  type: K
  path: IteratorPath<T>

  constructor (set: IterableSet<T>, type: K) {
    const node = set.tree
    const keys = Array.from(node.keys())
    this.set = set
    this.type = type
    this.path = keys.length > 0 ? [{ node, keys }] : []
  }

  next (): IteratorResult<Result<T, K>> {
    const value = this.dive()
    this.backtrack()
    return value
  }

  dive (): IteratorResult<Result<T, K>> {
    if (this.path.length === 0) { return { done: true, value: undefined } }
    const { node, keys } = last(this.path)!
    if (last(keys) === LEAF) { return { done: false, value: this.result() } }

    const child = node.get(last(keys)!)!
    this.path.push({ node: child, keys: Array.from(child.keys()) })
    return this.dive()
  }

  backtrack (): void {
    if (this.path.length === 0) { return }
    const keys = last(this.path)!.keys
    keys.pop()
    if (keys.length > 0) { return }
    this.path.pop()
    this.backtrack()
  }

  key (): string {
    return this.set.prefix + this.path
      .map(({ keys }) => last(keys))
      .filter(key => key !== LEAF)
      .join('')
  }

  value (): T {
    return last(this.path)!.node.get(LEAF)!
  }

  result (): Result<T, K> {
    switch (this.type) {
      case VALUES: return this.value() as Result<T, K>
      case KEYS: return this.key() as Result<T, K>
      default: return [this.key(), this.value()] as Result<T, K>
    }
  }

  [Symbol.iterator] () {
    return this
  }
}

const last = <T>(array: T[]): T | undefined => {
  return array[array.length - 1]
}

export { TreeIterator, ENTRIES, KEYS, VALUES, LEAF }
