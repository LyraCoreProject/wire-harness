#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import socket
import subprocess
import tarfile
import time
import zipfile

ADAPTER = Path(__file__).resolve().parent
ROOT = Path.home() / '.cache/lyracore-scratch/reference-server'
PINS = json.loads((ADAPTER / 'pins.json').read_text())
IMAGE = 'ubuntu:24.04@sha256:24737cef17f4e22b1977df7d9a05e77c4e28b7da5b8911f8d8ccda136bf3573c'
ENV = dict(os.environ, REFERENCE_ROOT=str(ROOT), REFERENCE_UID=str(os.getuid()), REFERENCE_GID=str(os.getgid()))
COMPOSE = ['docker', 'compose', '-f', str(ADAPTER / 'compose.yml')]


def run(args, **kwargs):
    return subprocess.run(args, check=True, env=ENV, **kwargs)


def secret_file(path, text):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, 'w') as file:
        file.write(text)
    path.chmod(0o600)


def digest(path):
    with path.open('rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()


def asset(kind):
    name = PINS[f'{kind}_asset']
    path = ROOT / 'downloads' / name
    cached = Path.home() / '.cache/lyracore-scratch/vmangos-db' / name
    if not path.exists() and kind == 'database' and cached.is_file():
        shutil.copyfile(cached, path)
    if not path.exists():
        run(['gh', 'release', 'download', PINS[f'{kind}_tag'], '--repo', 'vmangos/core', '--pattern', name, '--dir', str(path.parent)])
    if digest(path) != PINS[f'{kind}_sha256']:
        raise RuntimeError(f'{kind} archive checksum mismatch')
    return path


def prepare():
    ROOT.mkdir(parents=True, exist_ok=True)
    ROOT.chmod(0o700)
    for name in ['downloads', 'binaries', 'config', 'init', 'imports', 'data', 'logs', 'credentials', 'diagnostics']:
        (ROOT / name).mkdir(exist_ok=True)
    pin_file = ROOT / 'runtime-pins.json'
    if pin_file.exists() and json.loads(pin_file.read_text()) != PINS:
        raise RuntimeError('runtime pins changed; prepare a fresh Reference Server before recording')
    pin_file.write_text(json.dumps(PINS, indent=2) + '\n')
    archive = asset('binary')
    if not (ROOT / 'binaries/.verified').exists():
        with tarfile.open(archive) as tar:
            tar.extractall(ROOT / 'binaries', filter='data')
        (ROOT / 'binaries/.verified').write_text(PINS['binary_sha256'])
    elif (ROOT / 'binaries/.verified').read_text() != PINS['binary_sha256']:
        raise RuntimeError('binary pin changed; use a reviewed fresh runtime directory')
    archive = asset('database')
    with zipfile.ZipFile(archive) as zip:
        for name in ['logon', 'mangos', 'characters', 'logs']:
            matches = [n for n in zip.namelist() if Path(n).name == name + '.sql']
            if len(matches) != 1:
                raise RuntimeError(f'database archive does not contain one {name}.sql')
            with zip.open(matches[0]) as source, (ROOT / 'imports' / f'{name}.sql').open('wb') as target:
                shutil.copyfileobj(source, target)


def client_data(args):
    output = run([str(args.lyracore_cli.resolve()), 'config'], cwd=args.lyracore_checkout, capture_output=True, text=True).stdout
    match = re.search(r'^\s*client-data\s+(.+?)\s*$', output, re.MULTILINE)
    if not match:
        raise RuntimeError('lyracore config did not report client-data')
    data = Path(match[1]).resolve()
    if not data.is_dir() or not (data / 'dbc.MPQ').is_file():
        raise RuntimeError('client-data is not a build-5875 Data directory with dbc.MPQ')
    return data


def extractor(executable, args=(), data=None, input_text=None):
    command = ['docker', 'run', '--rm', '--user', f'{os.getuid()}:{os.getgid()}', '--cpus', '2', '--memory', '4g',
               '--mount', f'type=bind,src={ROOT}/binaries/vmangos-linux-amd64,dst=/opt/vmangos,readonly',
               '--mount', f'type=bind,src={ROOT}/data,dst=/work', '-w', '/work', '-e', 'LD_LIBRARY_PATH=/opt/vmangos/lib']
    if data:
        command += ['--mount', f'type=bind,src={data},dst=/work/Data,readonly']
    command += [IMAGE, f'/opt/vmangos/bin/Extractors/{executable}', *args]
    with (ROOT / 'logs' / f'{executable}.log').open('w') as log:
        run(command, input=input_text, text=True, stdout=log, stderr=subprocess.STDOUT)


def extract(data):
    marker = ROOT / 'data/.extracted'
    if marker.exists():
        if marker.read_text() != PINS['binary_sha256']:
            raise RuntimeError('extractor pin changed; regenerate client extracts')
        return
    print('Extracting maps, DBCs and collision models; logs are in the runtime directory.', flush=True)
    extractor('MapExtractor', data=data, input_text='y\nn\n\n')
    buildings = ROOT / 'data/Buildings'
    if buildings.exists():
        shutil.rmtree(buildings)
    extractor('VMapExtractor', data=data, input_text='\n\n')
    (ROOT / 'data/vmaps').mkdir(exist_ok=True)
    extractor('VMapAssembler', ['Buildings', 'vmaps'], input_text='\n\n')
    (ROOT / 'data/mmaps').mkdir(exist_ok=True)
    extractor('MoveMapGenerator', ['0', '--tile', '32,48', '--threads', '2', '--silent',
              '--offMeshInput', '/opt/vmangos/bin/Extractors/offmesh.txt',
              '--configInputPath', '/opt/vmangos/bin/Extractors/config.json'])
    (ROOT / 'data/5875').mkdir(exist_ok=True)
    dbc = ROOT / 'data/5875/dbc'
    if not dbc.exists():
        dbc.symlink_to('../dbc')
    for relative in ['maps/0004832.map', 'vmaps/000.vmtree', 'mmaps/000.mmap', 'mmaps/0004832.mmtile', '5875/dbc/Map.dbc']:
        if not (ROOT / 'data' / relative).is_file():
            raise RuntimeError(f'extractor did not produce {relative}')
    marker.write_text(PINS['binary_sha256'])


def configure():
    env_file = ROOT / 'db.env'
    if env_file.exists():
        password = env_file.read_text().strip().removeprefix('MARIADB_ROOT_PASSWORD=')
        if not re.fullmatch(r'[0-9a-f]{32}', password):
            raise RuntimeError('unexpected local database credential format')
    else:
        password = secrets.token_hex(16)
        secret_file(env_file, f'MARIADB_ROOT_PASSWORD={password}\n')
    init = '''#!/bin/bash
set -euo pipefail
export MYSQL_PWD="$MARIADB_ROOT_PASSWORD"
for database in realmd mangos characters logs; do
  mariadb -uroot -e "CREATE DATABASE $database DEFAULT CHARACTER SET utf8;"
  file=$database
  if [[ $database == realmd ]]; then file=logon; fi
  mariadb -uroot "$database" < "/imports/$file.sql"
done
mariadb -uroot <<'SQL'
CREATE USER 'conformance'@'%' IDENTIFIED BY 'DB_PASSWORD';
GRANT ALL ON realmd.* TO 'conformance'@'%';
GRANT ALL ON mangos.* TO 'conformance'@'%';
GRANT ALL ON characters.* TO 'conformance'@'%';
GRANT ALL ON logs.* TO 'conformance'@'%';
INSERT INTO realmd.realmlist (id,name,address,localAddress,port,icon,realmflags,timezone,allowedSecurityLevel,gamebuild_min,gamebuild_max) VALUES (1,'Conformance','127.0.0.1','127.0.0.1',18085,0,0,1,0,5875,5875);
SQL
'''.replace('DB_PASSWORD', password)
    (ROOT / 'init/10-import.sh').write_text(init)
    for server in ['realmd', 'mangosd']:
        source = ROOT / f'binaries/vmangos-linux-amd64/etc/{server}.conf.dist'
        text = source.read_text()
        values = {'BindIP': '"0.0.0.0"', 'LogsDir': '"/runtime/logs"'}
        if server == 'realmd':
            values.update({'LoginDatabaseInfo': f'"database;3306;conformance;{password};realmd"', 'StrictVersionCheck': '0'})
        else:
            values.update({'DataDir': '"/runtime/data"', 'Console.Enable': '0'})
            for key, database in [('Login', 'realmd'), ('World', 'mangos'), ('Character', 'characters'), ('Logs', 'logs')]:
                values[key + 'Database.Info'] = f'"database;3306;conformance;{password};{database}"'
        for key, value in values.items():
            text, count = re.subn(r'^' + re.escape(key) + r'\s*=.*$', key + ' = ' + value, text, flags=re.MULTILINE)
            if count != 1:
                raise RuntimeError(f'config key changed: {key}')
        secret_file(ROOT / f'config/{server}.conf', text)
    # MariaDB reads this after switching to its container uid.
    (ROOT / 'init/10-import.sh').chmod(0o644)


def sql(statement):
    return run([*COMPOSE, 'exec', '-T', 'database', 'sh', '-c', 'MYSQL_PWD="$MARIADB_ROOT_PASSWORD" mariadb -uroot -N -B'],
               input=statement, text=True, capture_output=True).stdout.strip()


def account():
    password_file = ROOT / 'credentials/gm-password'
    exists = sql("SELECT COUNT(*) FROM realmd.account WHERE username='CONFORMANCE';") == '1'
    if exists:
        if not password_file.exists():
            raise RuntimeError('local GM Account exists but its password file is missing')
        grant_gm()
        return
    password = secrets.token_hex(6).upper()
    salt = (secrets.randbits(255) | (1 << 255)).to_bytes(32, 'little')
    identity = hashlib.sha1(('CONFORMANCE:' + password).encode('ascii')).digest()
    exponent = int.from_bytes(hashlib.sha1(salt + identity).digest(), 'little')
    modulus = int('894B645E89E1535BBDAD5B8B290650530801B18EBFBF5E8FAB3C82872A3E9BB7', 16)
    verifier = pow(7, exponent, modulus)
    sql(f"INSERT INTO realmd.account (username,gmlevel,v,s) VALUES ('CONFORMANCE',3,'{verifier:X}','{int.from_bytes(salt, 'little'):X}');\n"
        "INSERT INTO realmd.realmcharacters (realmid,acctid,numchars) SELECT 1,id,0 FROM realmd.account WHERE username='CONFORMANCE';")
    secret_file(password_file, password + '\n')
    (ROOT / 'credentials/gm-account').write_text('CONFORMANCE\n')
    grant_gm()


def grant_gm():
    sql("REPLACE INTO realmd.account_access (id,gmlevel,RealmID) SELECT id,3,1 FROM realmd.account WHERE username='CONFORMANCE';")


def wait_for_port(port):
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(('127.0.0.1', port), timeout=1):
                return
        except OSError:
            time.sleep(0.25)
    raise RuntimeError(f'Reference Server did not open port {port}; inspect compose logs')


def main():
    parser = argparse.ArgumentParser(description='Prepare the disposable build-5875 Reference Server.')
    parser.add_argument('--lyracore-checkout', type=Path, required=True)
    parser.add_argument('--lyracore-cli', type=Path, required=True)
    args = parser.parse_args()
    run(['docker', 'info'], stdout=subprocess.DEVNULL)
    data = client_data(args)
    prepare()
    extract(data)
    configure()
    run([*COMPOSE, 'up', '-d', '--wait', '--wait-timeout', '180', 'database'])
    account()
    run([*COMPOSE, 'up', '-d', 'auth', 'world'])
    wait_for_port(13724)
    wait_for_port(18085)
    provenance = {key: PINS[key] for key in ['binary_sha256', 'source_commit', 'database_commit']}
    provenance.update(server='vmangos', binary_release=PINS['binary_tag'] + '/' + PINS['binary_asset'])
    (ROOT / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    print(f'Reference Server ready on 127.0.0.1:13724 and :18085. Runtime: {ROOT}')


if __name__ == '__main__':
    main()
