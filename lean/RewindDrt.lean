/-
RewindDrt — the differential-testing CLI for the RewindSpec mirror of
`rushi_common::rewind::active_ranges` (the `lean-verify` op=drt model
executable).

One scenario per input line; the Rust production mirror is
`verification/rewind-drt` (same protocol, documented there too).
The two sides
must agree on every input: a green DRT run is the regression gate
between the kernel-checked spec and its Rust implementation
(docs/rewind-fork-design.md, "Verification").

Input line — `END` plus a decimal prefix length, then zero or more
rewind triples, single-space separated:

    END <pos> [<seq>:<target>:<mode> ...]

  pos      decimal Nat      the prefix end (0-based length of the log)
  <triple> <seq>:<target>:<mode>   one rewind event; seq and target are
             decimal Nats, mode is a single bit: 0 = on (target stays),
             1 = before (target is excluded). The generator only emits
             valid chains (target < seq).

The output line is the active range list after
`RewindSpec.activeRanges`:

    EMPTY                       when there are no active ranges
    <lo>-<hi> <lo>-<hi> ...    otherwise, ranges in ascending order

A malformed line (or a missing input) prints `ERR` and exits 1.
-/

import RewindSpec

namespace RewindSpec.Drt

/-- Split a string on every occurrence of `d`; empty tokens are kept
    (the protocol relies on them, e.g. a bare `END 0`). Tokens are
    built in reverse for speed and re-reversed per token on the way
    out. -/
def splitOnChar (s : String) (d : Char) : List String :=
  let cs := s.toList
  let rec go (acc : List (List Char)) (cur : List Char) (rest : List Char) : List (List Char) :=
    match rest with
    | [] => (cur :: acc).reverse
    | c :: rest' =>
        if c = d then go (cur :: acc) [] rest' else go acc (c :: cur) rest'
  go [] [] cs |>.map (fun l => String.ofList l.reverse)

/-- A decimal digit character to its 0-9 value. -/
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

/-- A single-bit mode field: `0` is on mode, `1` is before mode. -/
def boolOfBit (s : String) : Option Bool :=
  if s = "0" then some false
  else if s = "1" then some true
  else none

/-- One rewind triple `<seq>:<target>:<mode>`; `none` on a malformed
    triple. -/
def parseRewind (tok : String) : Option RewindRef :=
  match splitOnChar tok ':' with
  | [s, t, m] =>
      match natOfDec s, natOfDec t, boolOfBit m with
      | some seq, some target, some before =>
          some { seq := seq, target := target, before := before }
      | _, _, _ => none
  | _ => none

/-- Every element of the list is a `some` value. -/
def allSome (ls : List (Option RewindRef)) : Bool :=
  match ls with
  | [] => true
  | o :: rest => o.isSome && allSome rest

/-- One DRT input line to (prefix end, rewind list); `none` on a
    malformed line. -/
def parseLine (line : String) : Option (Nat × List RewindRef) :=
  match splitOnChar line ' ' with
  | "END" :: posTok :: tripleToks =>
      match natOfDec posTok with
      | some p =>
          let parsed := tripleToks.map parseRewind
          if allSome parsed then
            some (p, parsed.map (fun o => o.getD { seq := 0, target := 0, before := false }))
          else
            none
      | none => none
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

/-- One range `(lo, hi)` as the output token `<lo>-<hi>`. -/
def rangeTok (lo hi : Nat) : String :=
  natToString lo ++ "-" ++ natToString hi

/-- Join a list of strings with single spaces; the empty list joins to
    the empty string, so there is no leading or trailing space. -/
def joinSpaced (ss : List String) : String :=
  match ss with
  | [] => ""
  | [s] => s
  | s :: rest => s ++ " " ++ joinSpaced rest

/-- The canonical range-list rendering: the DRT output line (no
    newline). `EMPTY` for the empty list, otherwise the ascending
    `<lo>-<hi>` tokens single-space joined. -/
def renderRanges (rs : List (Nat × Nat)) : String :=
  let toks := rs.map (fun pr => rangeTok pr.1 pr.2)
  if rs.isEmpty then
    "EMPTY"
  else
    joinSpaced toks

/-- Parse one scenario line, run the spec's mirror of
    `active_ranges` over it, and render the active range list. -/
def runScenario (line : String) : Option String :=
  match parseLine line with
  | some (pos, ws) => some (renderRanges (activeRanges pos ws))
  | none => none

end RewindSpec.Drt

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
  match RewindSpec.Drt.runScenario input with
  | some out => do
      IO.println out
      pure 0
  | none => do
      IO.println "ERR"
      pure 1
