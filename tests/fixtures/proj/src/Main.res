open Belt
module Arr = Belt.Array

/** Top-level entry point. */
@genType
let entry = () => 1

let (first, second) = (1, 2)

external evalRaw: string => unit = "eval"

let update = (msg: Types.msg, count: int) =>
  switch msg {
  | Types.Increment => count + 1
  | Types.Decrement(n) if n > 0 => count - n
  | Types.Reset => 0
  | _ => count
  }

module Inner = {
  /** Nested helper. */
  let helper = x => x * 2

  module Deep = {
    let deepValue = 42
  }
}

let unicodeString = "héllo — wörld ✓ 日本語"
