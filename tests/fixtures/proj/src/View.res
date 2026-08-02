@react.component
let make = (~name: string, ~count: int) => {
  <div className="wrap">
    <span> {React.string(name)} </span>
    <b> {React.int(count)} </b>
  </div>
}

let polyColor = (c: [#red | #green]) =>
  switch c {
  | #red => "r"
  | #green => "g"
  }
