/-
TuiStreamDrt — the differential-testing CLI for the TuiStreamSpec
reference renderer (the `lean-verify` op=drt model executable).

One scenario per input line; the Rust production mirror is
`bin/tui-stream-drt` (same protocol, documented there too). The two
sides must agree on every input: a green DRT run is the regression
gate between the kernel-checked spec and its Rust implementation
(docs/tui-streaming-response.md, "Verification").

Input line — five single-space-separated fields:

    FT SC DRAFT SETTLED RESPONSES

  FT         0 | 1            the initial follow-tail choice
  SC         decimal Nat      the initial scroll offset
  DRAFT      <n>:<hex>        the carried draft text
  SETTLED    k | k,t,...      k already-settled texts (t is a text
                              token; k=0 is the bare "0")
  RESPONSES  m | m,c,t,...    m responses, the i-th with c_i chunks
                              (one `<c>` per response, then c text
                              tokens; m=0 is the bare "0")

A text token is `<n>:<hex>`: `<n>` is the character count and `<hex>`
the even-length lowercase hex of the text bytes. The DRT generator
restricts chunk text to the ASCII alphabet a-j 0-9, so the byte
value is the character code on both sides.

Output line (the view after `TuiStreamSpec.runResponses`):

    follow=<0|1> scroll=<n> draft=<n>:<hex> settled=<k | k,t,...>

A malformed line (or a missing input) prints `ERR` and exits 1.
-/

import TuiStreamSpec

namespace TuiStreamSpec.Drt

/-- A lowercase hex digit character to its 0-15 value. -/
def hexDigitVal : Char → Option Nat :=
  fun c => match c with
  | '0' => some 0
  | '1' => some 1
  | '2' => some 2
  | '3' => some 3
  | '4' => some 4
  | '5' => some 5
  | '6' => some 6
  | '7' => some 7
  | '8' => some 8
  | '9' => some 9
  | 'a' => some 10
  | 'b' => some 11
  | 'c' => some 12
  | 'd' => some 13
  | 'e' => some 14
  | 'f' => some 15
  | _ => none

/-- Split a string on every occurrence of `d`; empty tokens are kept
    (the protocol relies on them, e.g. an empty text token `0:`).
    Tokens are built in reverse for speed and re-reversed per token on
    the way out. -/
def splitOnChar (s : String) (d : Char) : List String :=
  let cs := s.toList
  let rec go (acc : List (List Char)) (cur : List Char) (rest : List Char) : List (List Char) :=
    match rest with
    | [] => (cur :: acc).reverse
    | c :: rest' =>
        if c = d then go (cur :: acc) [] rest' else go acc (c :: cur) rest'
  go [] [] cs |>.map (fun l => String.ofList l.reverse)

/-- One decimal digit character to its value. -/
def decDigitVal : Char → Option Nat :=
  fun c => match c with
  | '0' => some 0
  | '1' => some 1
  | '2' => some 2
  | '3' => some 3
  | '4' => some 4
  | '5' => some 5
  | '6' => some 6
  | '7' => some 7
  | '8' => some 8
  | '9' => some 9
  | _ => none

/-- Decimal string to a Nat; `none` when empty or not all digits. -/
def natOfDec (s : String) : Option Nat :=
  match s.toList with
  | [] => none
  | _ :: _ =>
      let rec go (cs : List Char) (acc : Nat) : Option Nat :=
        match cs with
        | [] => some acc
        | c :: rest =>
            match decDigitVal c with
            | some d => go rest (acc * 10 + d)
            | none => none
      go s.toList 0

/-- Even-length lowercase hex string to byte values. -/
def hexToValues (s : String) : Option (List Nat) :=
  let rec go (cs : List Char) : Option (List Nat) :=
    match cs with
    | [] => some []
    | c1 :: c2 :: rest =>
        match hexDigitVal c1, hexDigitVal c2 with
        | some hi, some lo =>
            match go rest with
            | some r => some ((hi * 16 + lo) :: r)
            | none => none
        | _, _ => none
    | _ :: _ => none
  go s.toList

/-- The DRT chunk alphabet (a-j, 0-9) to byte values: the byte value
    is the character code on both sides of the gate. -/
def charVal : Char → Nat :=
  fun c => match c with
  | 'a' => 97
  | 'b' => 98
  | 'c' => 99
  | 'd' => 100
  | 'e' => 101
  | 'f' => 102
  | 'g' => 103
  | 'h' => 104
  | 'i' => 105
  | 'j' => 106
  | '0' => 48
  | '1' => 49
  | '2' => 50
  | '3' => 51
  | '4' => 52
  | '5' => 53
  | '6' => 54
  | '7' => 55
  | '8' => 56
  | '9' => 57
  | _ => 0

/-- One text token: `<n>:<hex>` — the byte values of `n` characters
    as even-length lowercase hex (empty hex means n = 0). -/
def parseTextTok (tok : String) : Option Text :=
  match splitOnChar tok ':' with
  | [n, hex] =>
      match natOfDec n with
      | some len =>
          match hexToValues hex with
          | some vals =>
              if len = vals.length then some (vals.map Char.ofNat) else none
          | none => none
      | none => none
  | _ => none

/-- Every element of the list is a `some` value. -/
def allSome (ls : List (Option Text)) : Bool :=
  match ls with
  | [] => true
  | o :: rest => o.isSome && allSome rest

/-- The settled-transcript field: `k` for an empty list, otherwise
    `k,<tok>,...` with k text tokens. -/
def parseSettledField (f : String) : Option (List Text) :=
  match splitOnChar f ',' with
  | [] => none
  | [first] =>
      match natOfDec first with
      | some 0 => some []
      | _ => none
  | first :: rest =>
      match natOfDec first with
      | some k =>
          if k = rest.length then
            if allSome (rest.map parseTextTok) then
              some (rest.map (fun t => parseTextTok t |>.getD []))
            else none
          else none
      | none => none

/-- The tail of the responses field: `m` responses to parse from the
    flat token stream `cs` (one count token plus its chunk tokens,
    repeated). Structural in `cs`: each step drops at least the count
    token. -/
def parseResponsesTail (m : Nat) (cs : List String) : Option (List (List Text)) :=
  match cs with
  | [] =>
      if m = 0 then some [] else none
  | cTok :: rest =>
      if m = 0 then
        none
      else
        match natOfDec cTok with
        | some c =>
            let toks := rest.take c
            if toks.length = c then
              if allSome (toks.map parseTextTok) then
                match parseResponsesTail (m - 1) (rest.drop c) with
                | some more =>
                    some ((toks.map (fun t => parseTextTok t |>.getD [])) :: more)
                | none => none
              else none
            else none
        | none => none
  termination_by cs.length

/-- The responses field: `m` for no responses, otherwise `m,<c>,...`
    — m responses, the i-th with c_i chunk text tokens. The flat token
    stream after the count is consumed by [`parseResponsesTail`]. -/
def parseResponsesField (f : String) : Option (List (List Text)) :=
  match splitOnChar f ',' with
  | [] => none
  | [first] =>
      match natOfDec first with
      | some 0 => some []
      | _ => none
  | first :: rest =>
      match natOfDec first with
      | some m => parseResponsesTail m rest
      | none => none

def boolOfBit (s : String) : Option Bool :=
  if s = "0" then some false
  else if s = "1" then some true
  else none

/-- One DRT input line to (initial view, response list); `none` on a
    malformed line. -/
def parseScenario (line : String) : Option (View × List (List Text)) :=
  match splitOnChar line ' ' with
  | [ft, sc, dr, se, rs] =>
      match boolOfBit ft, natOfDec sc, parseTextTok dr, parseSettledField se,
            parseResponsesField rs with
      | some ftB, some scN, some draft, some settled, some rs =>
          some ({ draft := draft, settled := settled, followTail := ftB, scroll := scN }, rs)
      | _, _, _, _, _ => none
  | _ => none

/-- Decimal digits of a byte-value nibble, as a string character. -/
def digitStr (d : Nat) : String :=
  match d with
  | 0 => "0"
  | 1 => "1"
  | 2 => "2"
  | 3 => "3"
  | 4 => "4"
  | 5 => "5"
  | 6 => "6"
  | 7 => "7"
  | 8 => "8"
  | 9 => "9"
  | _ => "0"

/-- Decimal digits of `n` (no leading zero: 0 is the empty string). -/
def natDigits (n : Nat) : String :=
  if n = 0 then
    ""
  else
    natDigits (n / 10) ++ digitStr (n % 10)
  termination_by n

/-- Nat to decimal string (core Lean has no `toString` for Nat); 0 is
    "0". -/
def natToString (n : Nat) : String :=
  let s := natDigits n
  if s = "" then "0" else s

/-- Two hex digits of a byte value (values below 256 only: the DRT
    domain is ASCII). -/
def hexVal (v : Nat) : String :=
  let h :=
    match v / 16 with
    | 0 => "0"
    | 1 => "1"
    | 2 => "2"
    | 3 => "3"
    | 4 => "4"
    | 5 => "5"
    | 6 => "6"
    | 7 => "7"
    | 8 => "8"
    | 9 => "9"
    | _ => "0"
  let l :=
    match v % 16 with
    | 0 => "0"
    | 1 => "1"
    | 2 => "2"
    | 3 => "3"
    | 4 => "4"
    | 5 => "5"
    | 6 => "6"
    | 7 => "7"
    | 8 => "8"
    | 9 => "9"
    | 10 => "a"
    | 11 => "b"
    | 12 => "c"
    | 13 => "d"
    | 14 => "e"
    | 15 => "f"
    | _ => "0"
  h ++ l

/-- The text-token encoding used by the output line. -/
def textTokOf (t : Text) : String :=
  natToString t.length ++ ":" ++
    (t.map charVal |>.map hexVal |>.foldl (fun (acc : String) s => acc ++ s) "")

/-- The settled field of the output line. -/
def settledFieldOf (ls : List Text) : String :=
  if ls.isEmpty then
    "0"
  else
    natToString ls.length ++
      (ls.map textTokOf |>.foldl (fun (acc : String) s => acc ++ "," ++ s) "")

/-- The canonical view rendering: the DRT output line (no newline). -/
def renderView (v : View) : String :=
  "follow=" ++ (if v.followTail then "1" else "0") ++
    " scroll=" ++ natToString v.scroll ++
    " draft=" ++ textTokOf v.draft ++
    " settled=" ++ settledFieldOf v.settled

/-- Parse one scenario line, run the spec's reference renderer over it
    (P1 convergence, P4 no-flush, and P5 append-only hold in the
    result), and render the final view. -/
def runScenario (line : String) : Option String :=
  match parseScenario line with
  | some (v, rs) => some (renderView (runResponses v rs))
  | none => none

end TuiStreamSpec.Drt

def main (args : List String) : IO UInt32 := do
  let fromEnvOpt ← IO.getEnv "DRT_INPUT"
  let fromEnv : String :=
    match fromEnvOpt with
    | some s => s
    | none => ""
  -- `args` are the CLI arguments (no program name): the DRT tool
  -- passes the input as the first argument, and also exports it as
  -- DRT_INPUT (same value); argv wins.
  let input :=
    match args with
    | s :: _ => s
    | [] => fromEnv
  match TuiStreamSpec.Drt.runScenario input with
  | some out => do
      IO.println out
      pure 0
  | none => do
      IO.println "ERR"
      pure 1
