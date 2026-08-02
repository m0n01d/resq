/** ReScript 12 constructs — agents must handle these, not just classic syntax. */
@unboxed
type value = Str(string) | Num(float)

type config = {name: string, retries?: int}

let lookup = dict{"a": 1, "b": 2}

let fetchAll = async (urls) => {
  let first = await fetch(urls)
  first
}

let rec even = x => x == 0 || odd(x - 1)
and odd = x => x != 0 && even(x - 1)

let pipeline = xs => xs->Array.map(x => x * 2)->Array.filter(x => x > 2)

let bigOne = 42n
