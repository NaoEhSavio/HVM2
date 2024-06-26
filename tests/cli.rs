//! Test the `hvmc` binary, including its CLI interface.

use std::{
  error::Error,
  io::Read,
  path::PathBuf,
  process::{Command, ExitStatus, Stdio},
};

use hvmc::{
  ast::{Net, Tree},
  host::Host,
};
use insta::assert_display_snapshot;

fn get_arithmetic_program_path() -> String {
  env!("CARGO_MANIFEST_DIR").to_owned() + "/examples/arithmetic.hvmc"
}

fn execute_hvmc(args: &[&str]) -> Result<(ExitStatus, String), Box<dyn Error>> {
  // Spawn the command
  let mut child =
    Command::new(env!("CARGO_BIN_EXE_hvmc")).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;

  // Capture the output of the command
  let mut stdout = child.stdout.take().ok_or("Couldn't capture stdout!")?;
  let mut stderr = child.stderr.take().ok_or("Couldn't capture stderr!")?;

  // Wait for the command to finish and get the exit status
  let status = child.wait()?;

  // Read the output
  let mut output = String::new();
  stdout.read_to_string(&mut output)?;
  stderr.read_to_string(&mut output)?;

  // Print the output of the command
  Ok((status, output))
}

#[test]
fn test_cli_reduce() {
  // Test normal-form expressions
  assert_display_snapshot!(
    execute_hvmc(&["reduce", "-m", "100M", "--", "#1"]).unwrap().1,
    @"#1"
  );
  // Test non-normal form expressions
  assert_display_snapshot!(
    execute_hvmc(&["reduce", "-m", "100M", "--", "a & #3 ~ <* #4 a>"]).unwrap().1,
    @"#12"
  );
  // Test multiple expressions
  assert_display_snapshot!(
    execute_hvmc(&["reduce", "-m", "100M", "--", "a & #3 ~ <* #4 a>", "a & #64 ~ </ #2 a>"]).unwrap().1,
    @"#12\n#32"
  );

  // Test loading file and reducing expression
  let arithmetic_program = get_arithmetic_program_path();

  assert_display_snapshot!(
    execute_hvmc(&[
      "reduce", "-m", "100M",
      &arithmetic_program,
      "--", "a & @mul ~ (#3 (#4 a))"
    ]).unwrap().1,
    @"#12"
  );

  assert_display_snapshot!(
    execute_hvmc(&[
      "reduce", "-m", "100M",
      &arithmetic_program,
      "--", "a & @mul ~ (#3 (#4 a))", "a & @div ~ (#64 (#2 a))"
    ]).unwrap().1,
    @"#12\n#32"
  )
}

#[test]
fn test_cli_run_with_args() {
  let arithmetic_program = get_arithmetic_program_path();

  // Test simple program running
  assert_display_snapshot!(
    execute_hvmc(&[
      "run", "-m", "100M",
      &arithmetic_program,
    ]).unwrap().1,
    @"({3 </ a b> <% c d>} ({5 a c} [b d]))"
  );

  // Test partial argument passing
  assert_display_snapshot!(
    execute_hvmc(&[
      "run", "-m", "100M",
      &arithmetic_program,
      "#64"
    ]).unwrap().1,
    @"({5 </$ #64 a> <%$ #64 b>} [a b])"
  );

  // Test passing all arguments.
  assert_display_snapshot!(
    execute_hvmc(&[
      "run", "-m", "100M",
      &arithmetic_program,
      "#64",
      "#3"
    ]).unwrap().1,
    @"[#21 #1]"
  );
}

#[test]
fn test_cli_transform() {
  let arithmetic_program = get_arithmetic_program_path();

  // Test simple program running
  assert_display_snapshot!(
    execute_hvmc(&[
      "transform",
      "-Opre-reduce",
      &arithmetic_program,
    ]).unwrap().1,
    @r###"
  @add = (<+ a b> (a b))

  @div = (</ a b> (a b))

  @main = ({3 </ a b> <% c d>} ({5 a c} [b d]))

  @mod = (<% a b> (a b))

  @mul = (<* a b> (a b))

  @sub = (<- a b> (a b))
  "###
  );

  assert_display_snapshot!(
    execute_hvmc(&[
      "transform",
      "-Opre-reduce",
      "--pre-reduce-skip", "main",
      &arithmetic_program,
    ]).unwrap().1,
    @r###"
  @add = (<+ a b> (a b))

  @div = (</ a b> (a b))

  @main = ({3 a b} ({5 c d} [e f]))
    & @mod ~ (b (d f))
    & @div ~ (a (c e))

  @mod = (<% a b> (a b))

  @mul = (<* a b> (a b))

  @sub = (<- a b> (a b))
  "###
  );

  // Test log

  assert_display_snapshot!(
    execute_hvmc(&[
      "transform",
      "-Opre-reduce",
      &(env!("CARGO_MANIFEST_DIR").to_owned() + "/tests/programs/log.hvmc")
    ]).unwrap().1,
    @r###"
  @main = a
    & @HVM.log ~ (#1 (#2 a))
  "###
  );
}

#[test]
fn test_cli_errors() {
  // Test passing all arguments.
  assert_display_snapshot!(
    execute_hvmc(&[
      "run", "this-file-does-not-exist.hvmc"
    ]).unwrap().1,
    @r###"
 Input file "this-file-does-not-exist.hvmc" not found
 "###
  );
  assert_display_snapshot!(
    execute_hvmc(&[
      "reduce", "this-file-does-not-exist.hvmc"
    ]).unwrap().1,
    @r###"
 Input file "this-file-does-not-exist.hvmc" not found
 "###
  );
}

#[test]
fn test_apply_tree() {
  use hvmc::run;
  fn eval_with_args(fun: &str, args: &[&str]) -> Net {
    let area = run::Heap::new_exact(16).unwrap();

    let mut fun: Net = fun.parse().unwrap();
    for arg in args {
      let arg: Tree = arg.parse().unwrap();
      fun.apply_tree(arg)
    }

    let host = Host::default();
    let mut rnet = run::Net::<run::Strict>::new(&area);
    let root_port = run::Trg::port(run::Port::new_var(rnet.root.addr()));
    host.encode_net(&mut rnet, root_port, &fun);
    rnet.normal();
    host.readback(&rnet)
  }
  assert_display_snapshot!(
    eval_with_args("(a a)", &["(a a)"]),
    @"(a a)"
  );
  assert_display_snapshot!(
    eval_with_args("b & (a b) ~ a", &["(a a)"]),
    @"a"
  );
  assert_display_snapshot!(
    eval_with_args("(z0 z0)", &["(z1 z1)"]),
    @"(a a)"
  );
  assert_display_snapshot!(
    eval_with_args("(* #1)", &["(a a)"]),
    @"#1"
  );
  assert_display_snapshot!(
    eval_with_args("(<+ a b> (a b))", &["#1", "#2"]),
    @"#3"
  );
  assert_display_snapshot!(
    eval_with_args("(<* a b> (a b))", &["#2", "#3"]),
    @"#6"
  );
  assert_display_snapshot!(
    eval_with_args("(<* a b> (a b))", &["#2"]),
    @"(<* #2 a> a)"
  );
}

#[test]
fn test_cli_compile() {
  // Test normal-form expressions

  if !Command::new(env!("CARGO_BIN_EXE_hvmc"))
    .args(["compile", &get_arithmetic_program_path()])
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .spawn()
    .unwrap()
    .wait()
    .unwrap()
    .success()
  {
    panic!("{:?}", "compilation failed");
  };

  let mut output_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  output_path.push("examples/arithmetic");
  let mut child = Command::new(&output_path).args(["#40", "#3"]).stdout(Stdio::piped()).spawn().unwrap();

  let mut stdout = child.stdout.take().ok_or("Couldn't capture stdout!").unwrap();
  child.wait().unwrap();
  let mut output = String::new();
  stdout.read_to_string(&mut output).unwrap();

  assert_display_snapshot!(output, @r###"
  [#13 #1]
  "###);

  std::fs::remove_file(&output_path).unwrap();
}
