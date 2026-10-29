#!/usr/bin/env python3
"""Build system-first Android packs and a Gradle-consumable AAR, without Gradle."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import time
import zipfile

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {'arm64-v8a': ('aarch64-linux-android', 'aarch64'), 'x86_64': ('x86_64-linux-android', 'x86_64')}

def run(*args, env=None):
    subprocess.run([str(a) for a in args], cwd=ROOT, env=env, check=True)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--abi', action='append', choices=TARGETS)
    parser.add_argument('--sdk', type=Path, default=Path(os.environ.get('ANDROID_HOME', ROOT / '.deps/android-sdk')))
    parser.add_argument('--ndk', type=Path)
    parser.add_argument('--java-home', type=Path)
    parser.add_argument('--debug', action='store_true')
    parser.add_argument('--clippy', action='store_true', help='Lint Android-only Rust code before building')
    parser.add_argument('--build-directory', type=Path, default=ROOT / 'target')
    parser.add_argument('--report', type=Path)
    parser.add_argument('--system-only', action='store_true', help='Omit the standalone offline SRC pack')
    parser.add_argument('--dynamic-plugins', action='store_true', help='Build the legacy multi-library package for comparison')
    args = parser.parse_args()
    started = time.monotonic()
    sdk = args.sdk.resolve()
    ndk = (args.ndk or Path(os.environ.get('ANDROID_NDK_HOME', sdk / 'ndk/28.2.13676358'))).resolve()
    java = (args.java_home or Path(os.environ.get('JAVA_HOME', ROOT / '.deps/android-jdk/Contents/Home'))).resolve()
    host = {'Darwin': 'darwin-x86_64', 'Linux': 'linux-x86_64', 'Windows': 'windows-x86_64'}[platform.system()]
    tools = ndk / 'toolchains/llvm/prebuilt' / host / 'bin'
    suffix = '.cmd' if os.name == 'nt' else ''
    profile = 'debug' if args.debug else 'release'
    work = ROOT / 'target/android' / profile
    work.mkdir(parents=True, exist_ok=True)
    classes = work / 'classes'
    shutil.rmtree(classes, ignore_errors=True)
    classes.mkdir()
    android_jar = sdk / 'platforms/android-35/android.jar'
    sources = sorted((ROOT / 'platforms/android/src/main/java').rglob('*.java'))
    run(java / 'bin/javac', '-Xlint:all,-options', '-Werror', '--release', '8', '-classpath', android_jar, '-d', classes, *sources)
    classes_jar = work / 'classes.jar'
    with zipfile.ZipFile(classes_jar, 'w', zipfile.ZIP_DEFLATED) as archive:
        for file in sorted(classes.rglob('*.class')):
            archive.write(file, file.relative_to(classes).as_posix())
    packs = {
        'native': ['native.audio.gain','native.video.rgba','native.wav.mux','native.packet.copy','native.mp3.mux','native.mp3.gapless_mux','native.file.sink'],
        'android': ['android.demux','android.demux.capabilities','android.audio.capabilities','android.video.capabilities','android.audio.decode','android.audio.convert','android.video.decode','android.audio.encode','android.mux'],
        'image': ['android.image.' + op for op in ['decode','probe','from_video','scale','crop','alpha','orient','tile','compose','encode','capabilities']],
    }
    if not args.system_only:
        packs['fallback_resample'] = ['fallback.audio.resample']
    packages = (['remdia-android-host'] + ['remdia-backend-' + pack.replace('_', '-') for pack in packs]) if args.dynamic_plugins else ['remdia-sdk']
    abis = args.abi or list(TARGETS)
    version = json.loads(subprocess.check_output(['cargo','metadata','--no-deps','--format-version','1','--locked'], cwd=ROOT))['packages'][0]['version']
    output = ROOT / 'dist/android'
    output.mkdir(parents=True, exist_ok=True)
    name = f'remdia-{version}' + ('-system-only' if args.system_only else '') + ('-dynamic' if args.dynamic_plugins else '') + ('-debug' if args.debug else '') + '.aar'
    report = {'profile': profile, 'system_only': args.system_only, 'layout':'dynamic' if args.dynamic_plugins else 'unified', 'clippy':args.clippy, 'abis': {}, 'ndk': ndk.name}
    with zipfile.ZipFile(output / name, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        archive.write(classes_jar, 'classes.jar')
        archive.write(ROOT / 'platforms/android/src/main/AndroidManifest.xml', 'AndroidManifest.xml')
        archive.write(ROOT / 'platforms/android/consumer-rules.pro', 'proguard.txt')
        for file in ['LICENSE', 'THIRD_PARTY.md']:
            archive.write(ROOT / file, 'assets/remdia/' + file)
        if not args.system_only:
            archive.write(ROOT / 'plugins/fallback-resample/vendor/LICENSE', 'assets/remdia/SpeexDSP-LICENSE.txt')
        for abi in abis:
            triple, arch = TARGETS[abi]
            env = os.environ.copy()
            env['CARGO_TARGET_DIR'] = str(args.build_directory.resolve())
            env['CARGO_TARGET_' + triple.upper().replace('-', '_') + '_LINKER'] = str(tools / (triple + '28-clang' + suffix))
            env['CC_' + triple.replace('-', '_')] = str(tools / (triple + '28-clang' + suffix))
            env['AR_' + triple.replace('-', '_')] = str(tools / ('llvm-ar.exe' if os.name == 'nt' else 'llvm-ar'))
            env['RUSTFLAGS'] = env.get('RUSTFLAGS', '') + ' -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384'
            command = ['cargo', 'build', '--locked', '--target', triple]
            if not args.dynamic_plugins:
                command.extend(['--no-default-features', '--features', 'remdia-sdk/bundled' + (',remdia-sdk/android-src' if not args.system_only else '')])
            if not args.debug: command.append('--release')
            for package in packages: command.extend(['-p', package])
            begin = time.monotonic()
            if args.clippy:
                run('cargo', 'clippy', *command[2:], '--', '-D', 'warnings', env=env)
            if args.dynamic_plugins:
                for package in packages:
                    dynamic = ['cargo','rustc','--locked','--target',triple,'-p',package,'--lib','--crate-type','cdylib']
                    if not args.debug: dynamic.append('--release')
                    run(*dynamic, env=env)
            else:
                run(*command, env=env)
            binaries = args.build_directory.resolve() / triple / profile
            manifests = []
            sizes = {}
            packaged = packs if args.dynamic_plugins else {'sdk':sorted({cap for capabilities in packs.values() for cap in capabilities})}
            for pack, capabilities in packaged.items():
                library = 'libremdia_sdk.so' if pack == 'sdk' else f'libremdia_backend_{pack}.so'
                manifests.append(dict(id=pack, abi_major=2, abi_minor=2, os='android', arch=arch, library=library, capabilities=capabilities))
                archive.write(binaries / library, f'jni/{abi}/{library}')
                sizes[library] = (binaries / library).stat().st_size
            if args.dynamic_plugins:
                library = 'libremdia_android_host.so'
                archive.write(binaries / library, f'jni/{abi}/{library}')
                sizes[library] = (binaries / library).stat().st_size
            archive.writestr(f'assets/remdia/{abi}/manifests.json', json.dumps(manifests, indent=2) + '\n')
            report['abis'][abi] = {'build_seconds': round(time.monotonic() - begin, 3), 'libraries': sizes, 'native_bytes': sum(sizes.values())}
    path = output / name
    report.update(aar_bytes=path.stat().st_size, total_seconds=round(time.monotonic() - started, 3), sha256=hashlib.sha256(path.read_bytes()).hexdigest())
    (output / (name + '.json')).write_text(json.dumps(report, indent=2) + '\n')
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + '\n')
    (output / 'SHA256SUMS.txt').write_text(''.join(hashlib.sha256(file.read_bytes()).hexdigest() + '  ' + file.name + '\n' for file in sorted(output.glob('*.aar'))))
    print(json.dumps(report, indent=2))
    print(path)

if __name__ == '__main__':
    main()
