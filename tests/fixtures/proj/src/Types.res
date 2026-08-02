/** The application message type. */
type msg =
  | Increment
  | Decrement(int)
  | Reset

/** A user record. */
type user = {name: string, age: int}

type id = string

type abstractThing
