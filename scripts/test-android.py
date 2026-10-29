#!/usr/bin/env python3
"""Build and run the offline Android certification APK on an explicit device."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parent.parent

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--aar', type=Path, required=True)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--abi', default='arm64-v8a', choices=['arm64-v8a', 'x86_64'])
    parser.add_argument('--expected-page-size', type=int, choices=[4096, 16384])
    parser.add_argument('--adb', type=Path, help='Use a specific platform-tools adb binary')
    parser.add_argument('--sdk', type=Path, default=Path(os.environ.get('ANDROID_HOME', ROOT / '.deps/android-sdk')))
    parser.add_argument('--java-home', type=Path, default=Path(os.environ.get('JAVA_HOME', ROOT / '.deps/android-jdk/Contents/Home')))
    args = parser.parse_args()
    sdk, java = args.sdk.resolve(), args.java_home.resolve()
    tools = sdk / 'build-tools/35.0.0'
    android = sdk / 'platforms/android-35/android.jar'
    work = ROOT / 'target/android/smoke' / ('device-' + re.sub(r'[^A-Za-z0-9_.-]', '_', args.serial))
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    assets = work / 'assets'
    fixture = assets / 'fixtures'
    fixture.mkdir(parents=True)
    for source in (ROOT / 'tests/fixtures').iterdir():
        if source.is_file() and source.suffix not in ('.md',): shutil.copy2(source, fixture / source.name)
    with zipfile.ZipFile(args.aar) as aar:
        manifests = json.loads(aar.read(f'assets/remdia/{args.abi}/manifests.json'))
        system_only = not any('fallback.audio.resample' in pack['capabilities'] for pack in manifests)
        native_hashes = {Path(name).name: hashlib.sha256(aar.read(name)).hexdigest() for name in aar.namelist() if name.startswith(f'jni/{args.abi}/')}
        for name in aar.namelist():
            if name.startswith('assets/') or name == 'classes.jar': aar.extract(name, work)
    classes = work / 'classes'
    classes.mkdir()
    env = os.environ.copy()
    env['JAVA_HOME'] = str(java)
    def run(*command):
        subprocess.run([str(a) for a in command], cwd=ROOT, env=env, check=True)
    run(java / 'bin/javac', '-Xlint:all,-options', '-Werror', '--release', '8', '-classpath', str(android) + os.pathsep + str(work / 'classes.jar'), '-d', classes,
        *sorted((ROOT / 'platforms/android/src/androidTest/java').rglob('*.java')))
    tests = work / 'tests.jar'
    with zipfile.ZipFile(tests, 'w', zipfile.ZIP_DEFLATED) as archive:
        for file in classes.rglob('*.class'): archive.write(file, file.relative_to(classes).as_posix())
    dex = work / 'dex'
    dex.mkdir()
    run(tools / 'd8', '--min-api', '28', '--lib', android, '--output', dex, work / 'classes.jar', tests)
    unsigned = work / 'unsigned.apk'
    run(tools / 'aapt2', 'link', '-I', android, '--manifest', ROOT / 'platforms/android/src/androidTest/AndroidManifest.xml', '-A', assets, '-o', unsigned)
    with zipfile.ZipFile(unsigned, 'a', zipfile.ZIP_STORED) as apk, zipfile.ZipFile(args.aar) as aar:
        for file in dex.glob('*.dex'): apk.write(file, file.name)
        for name in aar.namelist():
            if name.startswith('jni/' + args.abi + '/'):
                apk.writestr('lib/' + name[4:], aar.read(name))
    key = ROOT / 'target/android/test.keystore'
    if not key.exists():
        run(java / 'bin/keytool', '-genkeypair', '-keystore', key, '-storepass', 'android', '-keypass', 'android', '-alias', 'androiddebugkey', '-dname', 'CN=remdia offline test', '-keyalg', 'RSA', '-keysize', '2048', '-validity', '3650')
    aligned = work / 'aligned.apk'
    run(tools / 'zipalign', '-P', '16', '-f', '4', unsigned, aligned)
    apk = work / 'remdia-smoke.apk'
    run(tools / 'apksigner', 'sign', '--ks', key, '--ks-pass', 'pass:android', '--out', apk, aligned)
    adb = (args.adb or sdk / 'platform-tools/adb').resolve()
    run(adb, '-s', args.serial, 'install', '--no-incremental', '-r', apk)
    run(adb, '-s', args.serial, 'shell', 'run-as', 'dev.remdia.smoke', 'rm', '-f', 'files/report.json')
    run(adb, '-s', args.serial, 'shell', 'am', 'instrument', '-w', '-r', '-e', 'system_only', str(system_only).lower(), 'dev.remdia.smoke/dev.remdia.Smoke')
    raw = subprocess.check_output([str(adb), '-s', args.serial, 'exec-out', 'run-as', 'dev.remdia.smoke', 'cat', 'files/report.json'], env=env)
    report = json.loads(raw)
    report.update(aar_sha256=hashlib.sha256(args.aar.read_bytes()).hexdigest(), abi=args.abi, native_sha256=native_hashes)
    if args.expected_page_size and report.get('page_size') != args.expected_page_size:
        report.update(success=False, error=f"Expected page size {args.expected_page_size}, got {report.get('page_size')}")
    (work / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2, ensure_ascii=False))
    if not report.get('success'): raise SystemExit('Android certification failed')

if __name__ == '__main__': main()
