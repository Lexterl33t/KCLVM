use crate::assembler::KclvmAssembler;
use crate::assembler::KclvmLibAssembler;
use crate::assembler::LibAssembler;
use crate::temp_file;
use crate::Command;
use crate::{execute, runner::ExecProgramArgs};
use kclvm_ast::ast::{Module, Program};
use kclvm_config::settings::load_file;
use kclvm_parser::load_program;
use kclvm_sema::resolver::resolve_program;
use std::fs::create_dir_all;
use std::panic::catch_unwind;
use std::panic::set_hook;
use std::path::PathBuf;
use std::{
    collections::HashMap,
    fs::{self, File},
};
use tempfile::tempdir;

const TEST_CASES: &[&'static str; 5] = &[
    "init_check_order_0",
    "init_check_order_1",
    "normal_2",
    "type_annotation_not_full_2",
    "multi_vars_0",
];

const MULTI_FILE_TEST_CASES: &[&'static str; 6] = &[
    "multi_file_compilation/no_kcl_mod_file",
    "multi_file_compilation/relative_import",
    "multi_file_compilation/relative_import_as",
    "multi_file_compilation/import_abs_path/app-main",
    "multi_file_compilation/import_regular_module",
    "multi_file_compilation/import_regular_module_as",
];

const EXEC_PROG_ARGS_TEST_CASE: &[&'static str; 1] = &["exec_prog_args/default.json"];

const SETTINGS_FILE_TEST_CASE: &[&'static (&str, &str); 1] =
    &[&("settings_file/settings.yaml", "settings_file/settings.json")];

const EXPECTED_JSON_FILE_NAME: &str = "stdout.golden.json";
const TEST_CASE_PATH: &str = "./src/test_datas";
const KCL_FILE_NAME: &str = "main.k";
const MAIN_PKG_NAME: &str = "__main__";

/// Load test kcl file to ast.Program
fn load_test_program(filename: String) -> Program {
    let module = load_module(filename);
    construct_program(module)
}

/// Load test kcl file to ast.Module
fn load_module(filename: String) -> Module {
    kclvm_parser::parse_file(&filename, None).unwrap()
}

/// Construct ast.Program by ast.Module and default configuration.
/// Default configuration:
///     module.pkg = "__main__"
///     Program.root = "__main__"
///     Program.main = "__main__"
///     Program.cmd_args = []
///     Program.cmd_overrides = []
fn construct_program(mut module: Module) -> Program {
    module.pkg = MAIN_PKG_NAME.to_string();
    let mut pkgs_ast = HashMap::new();
    pkgs_ast.insert(MAIN_PKG_NAME.to_string(), vec![module]);
    Program {
        root: MAIN_PKG_NAME.to_string(),
        main: MAIN_PKG_NAME.to_string(),
        pkgs: pkgs_ast,
        cmd_args: vec![],
        cmd_overrides: vec![],
    }
}

fn construct_pkg_lib_path(
    prog: &Program,
    assembler: &KclvmAssembler,
    main_path: &str,
    suffix: String,
) -> Vec<PathBuf> {
    let cache_dir = assembler.construct_cache_dir(&prog.root);
    let mut result = vec![];
    for (pkgpath, _) in &prog.pkgs {
        if pkgpath == "__main__" {
            result.push(fs::canonicalize(format!("{}{}", main_path.to_string(), suffix)).unwrap());
        } else {
            result.push(cache_dir.join(format!("{}{}", pkgpath.clone(), suffix)));
        }
    }
    return result;
}

/// Load the expect result from stdout.golden.json
fn load_expect_file(filename: String) -> String {
    let f = File::open(filename).unwrap();
    let v: serde_json::Value = serde_json::from_reader(f).unwrap();
    v.to_string()
}

/// Format str by json str
fn format_str_by_json(str: String) -> String {
    let v: serde_json::Value = serde_json::from_str(&str).unwrap();
    v.to_string()
}

fn execute_for_test(kcl_path: &String) -> String {
    let plugin_agent = 0;
    let args = ExecProgramArgs::default();
    // Parse kcl file
    let program = load_test_program(kcl_path.to_string());
    // Generate libs, link libs and execute.
    execute(program, plugin_agent, &args).unwrap()
}

fn gen_libs_for_test(entry_file: &str, test_kcl_case_path: &str) {
    let args = ExecProgramArgs::default();
    let opts = args.get_load_program_options();

    let mut prog = load_program(&[&test_kcl_case_path], Some(opts)).unwrap();
    let scope = resolve_program(&mut prog);

    let assembler = KclvmAssembler::default();
    let prog_for_cache = prog.clone();

    let lib_paths = assembler.gen_libs(
        prog,
        scope,
        &(entry_file.to_string()),
        KclvmLibAssembler::LLVM,
    );

    let expected_pkg_paths = construct_pkg_lib_path(
        &prog_for_cache,
        &assembler,
        PathBuf::from(entry_file).to_str().unwrap(),
        Command::get_lib_suffix(),
    );
    assert_eq!(lib_paths.len(), expected_pkg_paths.len());
    for pkg_path in &expected_pkg_paths {
        assert_eq!(pkg_path.exists(), true);
    }

    let tmp_main_lib_path = fs::canonicalize(format!(
        "{}{}",
        entry_file.to_string(),
        Command::get_lib_suffix()
    ))
    .unwrap();
    assert_eq!(tmp_main_lib_path.exists(), true);

    KclvmLibAssembler::LLVM.clean_path(&tmp_main_lib_path.to_str().unwrap());
    assert_eq!(tmp_main_lib_path.exists(), false);
}

fn assemble_lib_for_test(
    entry_file: &str,
    test_kcl_case_path: &str,
    assembler: &KclvmLibAssembler,
) -> String {
    // default args and configuration
    let mut args = ExecProgramArgs::default();

    args.k_filename_list.push(test_kcl_case_path.to_string());
    let files = args.get_files();
    let opts = args.get_load_program_options();

    // parse and resolve kcl
    let mut program = load_program(&files, Some(opts)).unwrap();
    let scope = resolve_program(&mut program);

    // tmp file
    let temp_entry_file_path = &format!("{}.ll", entry_file);
    let temp_entry_file_lib = &format!("{}.{}", entry_file, Command::get_lib_suffix());

    // assemble libs
    assembler.assemble_lib(
        &program,
        scope.import_names.clone(),
        entry_file,
        temp_entry_file_path,
        temp_entry_file_lib,
    )
}

#[test]
fn test_kclvm_runner_execute() {
    for case in TEST_CASES {
        let kcl_path = &format!("{}/{}/{}", TEST_CASE_PATH, case, KCL_FILE_NAME);
        let expected_path = &format!("{}/{}/{}", TEST_CASE_PATH, case, EXPECTED_JSON_FILE_NAME);
        let result = execute_for_test(kcl_path);
        let expected_result = load_expect_file(expected_path.to_string());
        assert_eq!(expected_result, format_str_by_json(result));
    }
}

#[test]
fn test_kclvm_runner_execute_timeout() {
    set_hook(Box::new(|_| {}));
    let result_time_out = catch_unwind(|| {
        gen_libs_for_test(
            "test/no_exist_path/",
            "./src/test_datas/multi_file_compilation/import_abs_path/app-main/main.k",
        );
    });
    let timeout_panic_msg = "called `Result::unwrap()` on an `Err` value: Timeout";
    match result_time_out {
        Err(panic_err) => {
            if let Some(s) = panic_err.downcast_ref::<String>() {
                assert_eq!(s, timeout_panic_msg)
            }
        }
        _ => {
            unreachable!()
        }
    }
}

#[test]
fn test_assemble_lib_llvm() {
    for case in TEST_CASES {
        let temp_dir = tempdir().unwrap();
        let temp_dir_path = temp_dir.path().to_str().unwrap();
        let temp_entry_file = temp_file(temp_dir_path);

        let kcl_path = &format!("{}/{}/{}", TEST_CASE_PATH, case, KCL_FILE_NAME);
        let assembler = &KclvmLibAssembler::LLVM;

        let lib_file = assemble_lib_for_test(
            &format!("{}{}", temp_entry_file, "4assemble_lib"),
            kcl_path,
            assembler,
        );

        let lib_path = std::path::Path::new(&lib_file);
        assert_eq!(lib_path.exists(), true);
        assembler.clean_path(&lib_file);
        assert_eq!(lib_path.exists(), false);
    }
}

#[test]
fn test_gen_libs() {
    for case in MULTI_FILE_TEST_CASES {
        let temp_dir = tempdir().unwrap();
        let temp_dir_path = temp_dir.path().to_str().unwrap();
        let temp_entry_file = temp_file(temp_dir_path);

        let kcl_path = &format!("{}/{}/{}", TEST_CASE_PATH, case, KCL_FILE_NAME);
        gen_libs_for_test(&format!("{}{}", temp_entry_file, "4gen_libs"), kcl_path);
    }
}

#[test]
fn test_new_assembler_with_thread_count() {
    let assembler = KclvmAssembler::new_with_thread_count(5);
    assert_eq!(assembler.get_thread_count(), 5);
}

#[test]
fn test_new_assembler_with_thread_count_invalid() {
    set_hook(Box::new(|_| {}));
    let result_new = catch_unwind(|| {
        let _assembler_err = KclvmAssembler::new_with_thread_count(0);
    });
    let err_msg = "Internal error, please report a bug to us. The error message is: Illegal thread count in multi-file compilation";
    match result_new {
        Err(panic_err) => {
            if let Some(s) = panic_err.downcast_ref::<String>() {
                assert_eq!(s, err_msg);
            }
        }
        _ => {
            unreachable!()
        }
    }
}

#[test]
fn test_clean_path_for_genlibs() {
    let temp_dir = tempdir().unwrap();
    let temp_dir_path = temp_dir.path().to_str().unwrap();
    let tmp_file_path = &temp_file(temp_dir_path);

    create_dir_all(tmp_file_path).unwrap();

    let file_name = &format!("{}/{}", tmp_file_path, "test");
    let file_suffix = ".ll";

    File::create(file_name).unwrap();
    let path = std::path::Path::new(file_name);
    assert_eq!(path.exists(), true);

    KclvmAssembler::new().clean_path_for_genlibs(file_name, file_suffix);
    assert_eq!(path.exists(), false);

    let test1 = &format!("{}{}", file_name, ".test1.ll");
    let test2 = &format!("{}{}", file_name, ".test2.ll");
    File::create(test1).unwrap();
    File::create(test2).unwrap();
    let path1 = std::path::Path::new(test1);

    let path2 = std::path::Path::new(test2);
    assert_eq!(path1.exists(), true);
    assert_eq!(path2.exists(), true);

    KclvmAssembler::new().clean_path_for_genlibs(file_name, file_suffix);
    assert_eq!(path1.exists(), false);
    assert_eq!(path2.exists(), false);
}

#[test]
fn test_to_json_program_arg() {
    for case in EXEC_PROG_ARGS_TEST_CASE {
        let test_case_json_file = &format!("{}/{}", TEST_CASE_PATH, case);
        let expected_json_str = fs::read_to_string(test_case_json_file).unwrap();
        let exec_prog_args = ExecProgramArgs::default();
        assert_eq!(expected_json_str.trim(), exec_prog_args.to_json().trim());
    }
}

#[test]
fn test_from_str_program_arg() {
    for case in EXEC_PROG_ARGS_TEST_CASE {
        let test_case_json_file = &format!("{}/{}", TEST_CASE_PATH, case);
        let expected_json_str = fs::read_to_string(test_case_json_file).unwrap();
        let exec_prog_args = ExecProgramArgs::from_str(&expected_json_str);
        assert_eq!(expected_json_str.trim(), exec_prog_args.to_json().trim());
    }
}

#[test]
fn test_from_setting_file_program_arg() {
    for (case_yaml, case_json) in SETTINGS_FILE_TEST_CASE {
        let test_case_yaml_file = &format!("{}/{}", TEST_CASE_PATH, case_yaml);
        let settings_file = load_file(test_case_yaml_file);

        let test_case_json_file = &format!("{}/{}", TEST_CASE_PATH, case_json);
        let expected_json_str = fs::read_to_string(test_case_json_file).unwrap();

        let exec_prog_args = ExecProgramArgs::from(settings_file);
        assert_eq!(expected_json_str.trim(), exec_prog_args.to_json().trim());
    }
}
