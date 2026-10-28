#!/usr/bin/env python3
"""Compare Cargo stable, Cargo with Blaze's Rust, and Blaze on SDK source copies.

Pass a dedicated, checksum-verified Blaze installation (not ~/.cargo/bin).
No global Cargo configuration, production source, or normal target directory is
changed. Raw logs/build outputs stay under --output. Source copies live in a
separate temporary directory so no source is under a directory named target.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]


def run_text(command, cwd=None, env=None):
    return subprocess.check_output([str(x) for x in command], cwd=cwd, env=env,
                                   text=True, stderr=subprocess.STDOUT).strip()


def snapshot(destination):
    if destination.exists():
        raise ValueError(f'Refusing to overwrite an existing experiment: {destination}')
    destination.mkdir(parents=True)
    names = run_text(['git', 'ls-files', '-z'], ROOT).split('\0')
    digest = hashlib.sha256()
    for name in sorted(n for n in names if n):
        source = ROOT / name
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        digest.update(name.encode() + b'\0' + source.read_bytes())
    # Blaze must see a real repository, rather than an ignored directory under
    # the parent checkout. These private repositories are never pushed.
    subprocess.run(['git', 'init', '-q', str(destination)], check=True)
    subprocess.run(['git', '-C', str(destination), 'add', '.'], check=True)
    subprocess.run(['git', '-C', str(destination), '-c', 'user.name=Remdia Benchmark',
                    '-c', 'user.email=benchmark@invalid', 'commit', '-qm', 'Benchmark snapshot'], check=True)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--blaze', type=Path, required=True)
    parser.add_argument('--blaze-home', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--scope', choices=['cli', 'workspace', 'native'], default='cli')
    parser.add_argument('--variants', nargs='+', choices=['cargo-stable', 'cargo-matched', 'blaze'],
                        default=['cargo-stable', 'cargo-matched', 'blaze'])
    parser.add_argument('--profile', choices=['dev', 'release'], default='dev')
    parser.add_argument('--cold-only', action='store_true')
    parser.add_argument('--jobs', type=int, default=1)
    parser.add_argument('--rounds', type=int, default=5)
    parser.add_argument('--memory-limit-mib', type=int, default=6144)
    parser.add_argument('--timeout', type=int, default=600)
    args = parser.parse_args()
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        parser.error('this experiment currently measures macOS Apple Silicon only')
    if args.jobs < 1 or args.rounds < 1:
        parser.error('--jobs and --rounds must be positive')
    args.blaze = args.blaze.resolve(strict=True)
    output = args.output.resolve()
    if not output.is_relative_to(ROOT / 'target'):
        parser.error('--output must be under this checkout\'s target/')
    if not args.blaze.is_relative_to(ROOT / 'target'):
        parser.error('use a dedicated Blaze installation under target/, not a shared executable directory')
    home = args.blaze_home.resolve()
    if not home.is_relative_to(ROOT / 'target'):
        parser.error('--blaze-home must be dedicated to this experiment under target/')
    if output.exists():
        parser.error('use a new --output directory so cold builds cannot reuse artifacts')
    output.mkdir(parents=True)
    sources = Path(tempfile.mkdtemp(prefix='remdia-blaze-')).resolve()
    home.mkdir(parents=True, exist_ok=True)
    home.chmod(0o700)
    env = os.environ.copy()
    for name in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_TARGET_DIR',
                 'CARGO_BUILD_TARGET', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS'):
        env.pop(name, None)
    for name in list(env):
        if name.startswith(('BLAZE_', 'RA_JIT_')):
            env.pop(name)
    env.update(BLAZE_HOME=str(home), BLAZE_CACHE=str(output / 'cache'), BLAZE_REMOTE='off',
               BLAZE_MEMORY_MAX='4G', BLAZE_SPARE='0',
               CARGO_BUILD_JOBS=str(args.jobs), CMAKE_BUILD_PARALLEL_LEVEL=str(args.jobs),
               MACOSX_DEPLOYMENT_TARGET='15.0')
    subprocess.run([str(args.blaze), 'stop'], env=env, capture_output=True)
    toolchain = Path(run_text([args.blaze, 'toolchain', '--path'], env=env).splitlines()[-1])
    metadata = dict(sdk_revision=run_text(['git', 'rev-parse', 'HEAD'], ROOT),
                    source_tree=run_text(['git', 'rev-parse', 'HEAD^{tree}'], ROOT),
                    tracked_worktree_changes=run_text(['git', 'status', '--porcelain',
                                                       '--untracked-files=no'], ROOT),
                    blaze=run_text([args.blaze, '--version'], env=env),
                    cargo=run_text(['cargo', '-V']), rustc=run_text(['rustc', '-Vv']),
                    matched_rustc=run_text([toolchain / 'bin/rustc', '-Vv']),
                    scope=args.scope, profile=args.profile, cold_only=args.cold_only,
                    jobs=args.jobs, rounds=args.rounds,
                    memory_limit_mib=args.memory_limit_mib,
                    environment={key: env[key] for key in ('BLAZE_HOME', 'BLAZE_CACHE',
                        'BLAZE_REMOTE', 'BLAZE_MEMORY_MAX', 'BLAZE_SPARE', 'MACOSX_DEPLOYMENT_TARGET')},
                    blaze_executable_sha256=hashlib.sha256(args.blaze.read_bytes()).hexdigest(),
                    ra_jit_sha256=hashlib.sha256((args.blaze.parent / 'ra-jit').read_bytes()).hexdigest(),
                    hardware=run_text(['sysctl', '-n', 'machdep.cpu.brand_string']),
                    physical_memory_bytes=int(run_text(['sysctl', '-n', 'hw.memsize'])),
                    source_directory=str(sources), variants=[])
    (output / 'environment.json').write_text(json.dumps(metadata, indent=2) + '\n')
    for variant in args.variants:
        work = sources / variant
        build_target = output / variant / 'target'
        source_hash = snapshot(work)
        case_env = env.copy()
        case_env['CARGO_TARGET_DIR'] = str(build_target)
        if variant == 'cargo-matched':
            case_env.update(RUSTC=str(toolchain / 'bin/rustc'), RUSTDOC=str(toolchain / 'bin/rustdoc'),
                            PATH=str(toolchain / 'bin') + os.pathsep + env['PATH'])
            builder = str(toolchain / 'bin/cargo')
        else:
            builder = str(args.blaze) if variant == 'blaze' else shutil.which('cargo')
        selection = {'cli': ['-p', 'remdia-cli'], 'workspace': ['--workspace'],
                     'native': ['-p', 'remdia-backend-native']}[args.scope]
        command = [builder, 'build', *selection, '--locked', '--offline']
        if args.profile == 'release':
            command += ['--release']
        case = dict(name=variant, source_sha256=source_hash, command=command, complete=False, runs=[])
        metadata['variants'].append(case)

        def measure(label):
            file = output / 'measurements' / (variant + '-' + label + '.json')
            probe = ['python3', str(ROOT / 'scripts/measure-command.py'),
                     '--label', variant + '-' + label, '--output', str(file), '--cwd', str(work),
                     '--memory-limit-mib', str(args.memory_limit_mib), '--timeout', str(args.timeout)]
            if variant == 'blaze':
                probe += ['--track-executable-dir', str(args.blaze.parent)]
            result = subprocess.run([*probe, '--', *command], env=case_env, capture_output=True, text=True)
            if not file.exists():
                raise RuntimeError(result.stderr + result.stdout)
            record = json.loads(file.read_text())
            log = file.with_suffix('.log').read_text()
            record['engine_finished'] = ('[blaze]' in log or '[ra-jit]' in log) if variant == 'blaze' else None
            case['runs'].append(record)
            print(f"{variant} {label}: {record['wall_seconds']:.3f}s, "
                  f"RSS {record['peak_tree_rss_bytes']/1024**2:.0f} MiB, "
                  f"footprint {(record['peak_tree_footprint_bytes'] or 0)/1024**2:.0f} MiB, "
                  f"exit {record['exit_code']}, abort={record['aborted']}", flush=True)
            return (record['exit_code'] == 0 and not record['aborted']
                    and not record['monitor_errors']
                    and (variant != 'blaze' or record['engine_finished']))

        source = work / ('plugins/native/src/lib.rs' if args.scope == 'native'
                         else 'crates/remdia-cli/src/main.rs')
        original = source.read_text()
        artifact_dir = build_target / ('release' if args.profile == 'release' else 'debug')

        def edit(number):
            if args.scope == 'native':
                source.write_text(original + '\n#[unsafe(no_mangle)]\n'
                    f'pub extern "C" fn remdia_benchmark_marker() -> u32 {{ {number} }}\n')
            else:
                source.write_text(original.replace('fn main() -> Result<()> {',
                    'fn main() -> Result<()> {\n'
                    f'    eprintln!("remdia-build-probe:{number:04d}");', 1))

        def verify(number):
            if args.scope == 'native':
                library = artifact_dir / 'libremdia_backend_native.dylib'
                code = ('import ctypes,sys; lib=ctypes.CDLL(sys.argv[1]); '
                        'assert lib.remdia_benchmark_marker()==int(sys.argv[2])')
                subprocess.run(['python3', '-c', code, str(library), str(number)], check=True)
            else:
                result = subprocess.run([str(artifact_dir / 'remdia'), '--version'],
                                        capture_output=True, text=True, check=True)
                marker = f'remdia-build-probe:{number:04d}'
                if result.stderr.strip() != marker:
                    raise RuntimeError(f'Stale executable: expected {marker!r}, got {result.stderr!r}')
            case['runs'][-1]['artifact_marker_verified'] = number

        try:
            if measure('cold'):
                if args.scope != 'native':
                    case['cold_artifact_version'] = run_text([artifact_dir / 'remdia', '--version'])
                if args.cold_only:
                    case['complete'] = True
                    continue
                for i in range(args.rounds):
                    if not measure(f'noop-{i}'):
                        raise RuntimeError('No-op build failed')
                edit(0)
                if not measure('prime-edit'):
                    raise RuntimeError('Priming edit failed')
                verify(0)
                for i in range(1, args.rounds + 1):
                    edit(i)
                    if not measure(f'edit-{i}'):
                        raise RuntimeError('Edited build failed')
                    verify(i)
                source.write_text(original)
                if not measure('restore'):
                    raise RuntimeError('Source restoration build failed')
                case['complete'] = True
        except (RuntimeError, subprocess.CalledProcessError) as error:
            case['correctness_or_build_error'] = str(error)
        finally:
            source.write_text(original)
            if variant == 'blaze':
                case['status'] = run_text([args.blaze, 'status'], env=env)
                subprocess.run([str(args.blaze), 'stop'], env=env, capture_output=True)
            for phase in ('noop', 'edit'):
                times = [r['wall_seconds'] for r in case['runs'] if
                         r['label'].startswith(variant + '-' + phase + '-') and r['exit_code'] == 0]
                case[phase + '_median_seconds'] = statistics.median(times) if times else None
            case['target_allocated_kib'] = (int(run_text(['du', '-sk', build_target]).split()[0])
                                            if build_target.exists() else 0)
            (output / 'summary.json').write_text(json.dumps(metadata, indent=2) + '\n')
    # Retain temporary snapshots for inspecting failed builds. They contain no
    # caller changes and can be removed using source_directory in summary.json.
    print(output / 'summary.json')
    return 0 if all(case['complete'] for case in metadata['variants']) else 1


if __name__ == '__main__':
    raise SystemExit(main())
