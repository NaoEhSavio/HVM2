#![cfg_attr(feature = "trace", feature(const_type_name))]

use clap::{Args, Parser, Subcommand};
use hvmc::{
  ast::{Book, Net, Tree},
  host::Host,
  run::{DynNet, Mode, Trg},
  stdlib::create_host,
  transform::{TransformOpts, TransformPass, TransformPasses},
  *,
};

use parking_lot::Mutex;
use std::{
  fs, io,
  path::Path,
  process::{self, Stdio},
  str::FromStr,
  sync::Arc,
  time::{Duration, Instant},
};

fn main() {
  if cfg!(feature = "trace") {
    trace::set_hook();
  }
  if cfg!(feature = "_full_cli") {
    let cli = FullCli::parse();
    match cli.mode {
      CliMode::Compile { file, transform_args, output } => {
        let output = output.as_deref().or_else(|| file.strip_suffix(".hvmc")).unwrap_or_else(|| {
          eprintln!("file missing `.hvmc` extension; explicitly specify an output path with `--output`.");
          process::exit(1);
        });
        let host = create_host(&load_book(&[file.clone()], &transform_args));
        compile_executable(output, host).unwrap();
      }
      CliMode::Run { run_opts, mut transform_args, file, args } => {
        // Don't pre-reduce or prune the entry point
        transform_args.transform_opts.pre_reduce_skip.push(args.entry_point.clone());
        transform_args.transform_opts.prune_entrypoints.push(args.entry_point.clone());
        let host = create_host(&load_book(&[file], &transform_args));
        run(host, run_opts, args);
      }
      CliMode::Reduce { run_opts, transform_args, files, exprs } => {
        let host = create_host(&load_book(&files, &transform_args));
        let exprs: Vec<_> = exprs.iter().map(|x| Net::from_str(x).unwrap()).collect();
        reduce_exprs(host, &exprs, &run_opts);
      }
      CliMode::Transform { transform_args, files } => {
        let book = load_book(&files, &transform_args);
        println!("{}", book);
      }
    }
  } else {
    let cli = BareCli::parse();
    let host = create_host(&Book::default());
    gen::insert_into_host(&mut host.lock());
    run(host, cli.opts, cli.args);
  }
  if cfg!(feature = "trace") {
    hvmc::trace::_read_traces(usize::MAX);
  }
}

#[derive(Parser, Debug)]
#[command(
  author,
  version,
  about = "A massively parallel Interaction Combinator evaluator",
  long_about = r##"
A massively parallel Interaction Combinator evaluator

Examples: 
$ hvmc run examples/church_encoding/church.hvm
$ hvmc run examples/addition.hvmc "#16" "#3"
$ hvmc compile examples/addition.hvmc
$ hvmc reduce examples/addition.hvmc -- "a & @mul ~ (#3 (#4 a))"
$ hvmc reduce -- "a & #3 ~ <* #4 a>""##
)]
struct FullCli {
  #[command(subcommand)]
  pub mode: CliMode,
}

#[derive(Parser, Debug)]
#[command(author, version)]
struct BareCli {
  #[command(flatten)]
  pub opts: RuntimeOpts,
  #[command(flatten)]
  pub args: RunArgs,
}

#[derive(Subcommand, Clone, Debug)]
#[command(author, version)]
enum CliMode {
  /// Compile a hvm-core program into a Rust crate.
  Compile {
    /// hvm-core file to compile.
    file: String,
    #[arg(short = 'o', long = "output")]
    /// Output path; defaults to the input file with `.hvmc` stripped.
    output: Option<String>,
    #[command(flatten)]
    transform_args: TransformArgs,
  },
  /// Run a program, optionally passing a list of arguments to it.
  Run {
    /// Name of the file to load.
    file: String,
    #[command(flatten)]
    args: RunArgs,
    #[command(flatten)]
    run_opts: RuntimeOpts,
    #[command(flatten)]
    transform_args: TransformArgs,
  },
  /// Reduce hvm-core expressions to their normal form.
  ///
  /// The expressions are passed as command-line arguments.
  /// It is also possible to load files before reducing the expression,
  /// which makes it possible to reference definitions from the file
  /// in the expression.
  Reduce {
    #[arg(required = false)]
    /// Files to load before reducing the expressions.
    ///
    /// Multiple files will act as if they're concatenated together.
    files: Vec<String>,
    #[arg(required = false, last = true)]
    /// Expressions to reduce.
    ///
    /// The normal form of each expression will be
    /// printed on a new line. This list must be separated from the file list
    /// with a double dash ('--').
    exprs: Vec<String>,
    #[command(flatten)]
    run_opts: RuntimeOpts,
    #[command(flatten)]
    transform_args: TransformArgs,
  },
  /// Transform a hvm-core program using one of the optimization passes.
  Transform {
    /// Files to load before reducing the expressions.
    ///
    /// Multiple files will act as if they're concatenated together.
    #[arg(required = true)]
    files: Vec<String>,
    #[command(flatten)]
    transform_args: TransformArgs,
  },
}

#[derive(Args, Clone, Debug)]
struct TransformArgs {
  /// Enables or disables transformation passes.
  #[arg(short = 'O', value_delimiter = ' ', action = clap::ArgAction::Append)]
  transform_passes: Vec<TransformPass>,

  #[command(flatten)]
  transform_opts: TransformOpts,
}

#[derive(Args, Clone, Debug)]
struct RuntimeOpts {
  #[arg(short = 's', long = "stats")]
  /// Show performance statistics.
  show_stats: bool,
  #[arg(short = '1', long = "single")]
  /// Single-core mode (no parallelism).
  single_core: bool,
  #[arg(short = 'l', long = "lazy")]
  /// Lazy mode.
  ///
  /// Lazy mode only expands references that are reachable
  /// by a walk from the root of the net. This leads to a dramatic slowdown,
  /// but allows running programs that would expand indefinitely otherwise.
  lazy_mode: bool,
  #[arg(short = 'm', long = "memory", value_parser = util::parse_abbrev_number::<usize>)]
  /// How much memory to allocate on startup.
  ///
  /// Supports abbreviations such as '4G' or '400M'.
  memory: Option<usize>,
}

#[derive(Args, Clone, Debug)]
struct RunArgs {
  #[arg(short = 'e', default_value = "main")]
  /// Name of the definition that will get reduced.
  entry_point: String,
  /// List of arguments to pass to the program.
  ///
  /// Arguments are passed using the lambda-calculus interpretation
  /// of interaction combinators. So, for example, if the arguments are
  /// "#1" "#2" "#3", then the expression that will get reduced is
  /// `r & @main ~ (#1 (#2 (#3 r)))`.
  args: Vec<String>,
}

fn run(host: Arc<Mutex<Host>>, opts: RuntimeOpts, args: RunArgs) {
  let mut net = Net { root: Tree::Ref { nam: args.entry_point }, redexes: vec![] };
  for arg in args.args {
    let arg: Net = Net::from_str(&arg).unwrap();
    net.redexes.extend(arg.redexes);
    net.apply_tree(arg.root);
  }

  reduce_exprs(host, &[net], &opts);
}

fn load_book(files: &[String], transform_args: &TransformArgs) -> Book {
  let mut book = files
    .iter()
    .map(|name| {
      let contents = fs::read_to_string(name).unwrap_or_else(|_| {
        eprintln!("Input file {:?} not found", name);
        process::exit(1);
      });
      contents.parse::<Book>().unwrap_or_else(|e| {
        eprintln!("Parsing error {e}");
        process::exit(1);
      })
    })
    .fold(Book::default(), |mut acc, i| {
      acc.nets.extend(i.nets);
      acc
    });

  let transform_passes = TransformPasses::from(&transform_args.transform_passes[..]);
  book.transform(transform_passes, &transform_args.transform_opts).unwrap();

  book
}

fn reduce_exprs(host: Arc<Mutex<Host>>, exprs: &[Net], opts: &RuntimeOpts) {
  let heap = run::Heap::new(opts.memory).expect("memory allocation failed");
  for expr in exprs {
    let mut net = DynNet::new(&heap, opts.lazy_mode);
    dispatch_dyn_net!(&mut net => {
      host.lock().encode_net(net, Trg::port(run::Port::new_var(net.root.addr())), expr);
      let start_time = Instant::now();
      if opts.single_core {
        net.normal();
      } else {
        net.parallel_normal();
      }
      let elapsed = start_time.elapsed();
      println!("{}", host.lock().readback(net));
      if opts.show_stats {
        print_stats(net, elapsed);
      }
    });
  }
}

fn print_stats<M: Mode>(net: &run::Net<M>, elapsed: Duration) {
  eprintln!("RWTS   : {:>15}", pretty_num(net.rwts.total()));
  eprintln!("- ANNI : {:>15}", pretty_num(net.rwts.anni));
  eprintln!("- COMM : {:>15}", pretty_num(net.rwts.comm));
  eprintln!("- ERAS : {:>15}", pretty_num(net.rwts.eras));
  eprintln!("- DREF : {:>15}", pretty_num(net.rwts.dref));
  eprintln!("- OPER : {:>15}", pretty_num(net.rwts.oper));
  eprintln!("TIME   : {:.3?}", elapsed);
  eprintln!("RPS    : {:.3} M", (net.rwts.total() as f64) / (elapsed.as_millis() as f64) / 1000.0);
}

fn pretty_num(n: u64) -> String {
  n.to_string()
    .as_bytes()
    .rchunks(3)
    .rev()
    .map(|x| std::str::from_utf8(x).unwrap())
    .flat_map(|x| ["_", x])
    .skip(1)
    .collect()
}

fn compile_executable(target: &str, host: Arc<Mutex<host::Host>>) -> Result<(), io::Error> {
  let gen = compile::compile_host(&host.lock());
  let outdir = ".hvm";
  if Path::new(&outdir).exists() {
    fs::remove_dir_all(outdir)?;
  }
  let cargo_toml = include_str!("../Cargo.toml");
  let mut cargo_toml = cargo_toml.split_once("##--COMPILER-CUTOFF--##").unwrap().0.to_owned();
  cargo_toml.push_str("[features]\ndefault = ['cli']\ncli = ['std', 'dep:clap']\nstd = []");

  macro_rules! include_files {
    ($([$($prefix:ident)*])? $mod:ident {$($sub:tt)*} $($rest:tt)*) => {
      fs::create_dir_all(concat!(".hvm/src/", $($(stringify!($prefix), "/",)*)? stringify!($mod)))?;
      include_files!([$($($prefix)* $mod)?] $($sub)*);
      include_files!([$($($prefix)*)?] $mod $($rest)*);
    };
    ($([$($prefix:ident)*])? $file:ident $($rest:tt)*) => {
      fs::write(
        concat!(".hvm/src/", $($(stringify!($prefix), "/",)*)* stringify!($file), ".rs"),
        include_str!(concat!($($(stringify!($prefix), "/",)*)* stringify!($file), ".rs")),
      )?;
      include_files!([$($($prefix)*)?] $($rest)*);
    };
    ($([$($prefix:ident)*])?) => {};
  }

  fs::create_dir_all(".hvm/src")?;
  fs::write(".hvm/Cargo.toml", cargo_toml)?;
  fs::write(".hvm/src/gen.rs", gen)?;

  include_files! {
    ast
    compile
    fuzz
    host {
      calc_labels
      encode
      readback
    }
    lib
    main
    ops {
      num
      word
    }
    prelude
    run {
      addr
      allocator
      def
      dyn_net
      instruction
      interact
      linker
      net
      node
      parallel
      port
      wire
    }
    stdlib
    trace
    transform {
      coalesce_ctrs
      encode_adts
      eta_reduce
      inline
      pre_reduce
      prune
    }
    util {
      apply_tree
      array_vec
      bi_enum
      create_var
      deref
      maybe_grow
      parse_abbrev_number
      stats
    }
  }

  let output = process::Command::new("cargo")
    .current_dir(".hvm")
    .arg("build")
    .arg("--release")
    .stderr(Stdio::inherit())
    .output()?;
  if !output.status.success() {
    process::exit(1);
  }

  fs::copy(".hvm/target/release/hvmc", target)?;

  Ok(())
}
