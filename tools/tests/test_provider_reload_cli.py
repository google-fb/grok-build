"""Provider reload over real ACP stdio; synthetic credentials and servers only.

Run with GROK_TEST_BINARY in a network-disabled container. Fresh sessions after
reload test the model catalog without assuming active-session snapshot semantics.
"""
import contextlib
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time
import unittest

import test_provider_cli as protocol
from test_request_usage_cli import AccountingHandler


class ReloadHandler(AccountingHandler):
    def do_POST(self):
        if not self.path.endswith('/chat/completions'):
            return super().do_POST()
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
        self.server.owner.requests.append({
            'port': self.server.server_port, 'body': body,
            'authorization': self.headers.get('Authorization'),
        })
        chunk = {'id': 'synthetic-reload', 'object': 'chat.completion.chunk',
                 'created': 0, 'model': body['model'],
                 'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': 'Done.'},
                              'finish_reason': 'stop'}],
                 'usage': {'prompt_tokens': 10, 'completion_tokens': 1, 'total_tokens': 11,
                           'prompt_tokens_details': {'cached_tokens': 0, 'cache_write_tokens': 0},
                           'completion_tokens_details': {'reasoning_tokens': 0}, 'cost': .001}}
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        self.wfile.write(('data: '+json.dumps(chunk)+'\n\ndata: [DONE]\n\n').encode())
        self.wfile.flush()


def config(port=28081, key='SYNTHETIC_A_KEY', provider='provider-a', model='model-a'):
    return f'''[model_providers.{provider}]
base_url = "http://127.0.0.1:{port}/v1"
provider_profile = "openrouter"
env_key = "{key}"
[model.{model}]
model_provider = "{provider}"
model = "synthetic-{model}"
context_window = 32768
'''


class Rpc:
    def __init__(self, binary, root):
        self.root = root
        self.messages = queue.Queue()
        self.next_id = 0
        self.errors = (root/'stderr.log').open('w')
        env = protocol.isolated_env(root, 'http://127.0.0.1:28182/v1')
        env.update(SYNTHETIC_A_KEY='synthetic-a-key', SYNTHETIC_B_KEY='synthetic-b-key')
        self.process = subprocess.Popen([str(binary), 'agent', '--no-leader', '--model', 'model-a', 'stdio'],
            cwd=root/'work', env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.errors, text=True, bufsize=1)
        def read():
            try:
                for line in self.process.stdout:
                    self.messages.put(json.loads(line))
            except Exception as error:
                self.messages.put(error)
            finally:
                self.messages.put(EOFError('ACP stdout closed'))
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()

    def request(self, method, params, allow_error=False):
        self.next_id += 1
        ident = self.next_id
        self.process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': ident,
                                            'method': method, 'params': params})+'\n')
        self.process.stdin.flush()
        deadline = time.monotonic()+30
        while True:
            try:
                message = self.messages.get(timeout=max(.01, deadline-time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'ACP timeout: {method}; '+(self.root/'stderr.log').read_text()[-2000:])
            if isinstance(message, Exception):
                raise AssertionError(f'ACP stopped: {message}; '+(self.root/'stderr.log').read_text()[-2000:])
            if message.get('id') == ident:
                if 'error' in message and not allow_error:
                    raise AssertionError(f'{method}: {message["error"]}')
                return message
            if 'id' in message and 'method' in message:
                raise AssertionError(f'Unexpected agent request: {message["method"]}')
            if time.monotonic() >= deadline:
                raise AssertionError(f'ACP notification loop: {method}')

    def prompt_new(self, model='model-a'):
        created = self.request('session/new', {'cwd': str(self.root/'work'), 'mcpServers': [],
            '_meta': {'modelId': model}})['result']
        return self.request('session/prompt', {'sessionId': created['sessionId'],
            'prompt': [{'type': 'text', 'text': 'Synthetic reload check.'}]})

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=3)
        self.reader.join(2)
        self.process.stdout.close()
        self.errors.close()


class ProviderReloadCli(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = Path(os.environ['GROK_TEST_BINARY']).resolve(strict=True)

    @contextlib.contextmanager
    def running(self, initial=None):
        with tempfile.TemporaryDirectory(prefix='astra-provider-reload-') as folder:
            root = Path(folder)
            for name in ('home', 'grok', 'work', 'tmp'):
                (root/name).mkdir()
            path = root/'grok/config.toml'
            path.write_text(initial or config())
            self.requests, self.aux_requests = [], []
            servers, threads = [], []
            rpc = None
            try:
                for port in (28081, 28181, 28182):
                    server = protocol.Server(('127.0.0.1', port), ReloadHandler)
                    server.owner = self
                    servers.append(server)
                    thread = threading.Thread(target=server.serve_forever, daemon=True)
                    thread.start()
                    threads.append(thread)
                rpc = Rpc(self.binary, root)
                rpc.request('initialize', {'protocolVersion': 1, 'clientCapabilities': {},
                    'clientInfo': {'name': 'synthetic-reload-test', 'version': '1'}})
                rpc.prompt_new()
                self.check_request(0, 28081, 'synthetic-a-key', 'synthetic-model-a')
                yield path, rpc
            finally:
                if rpc is not None:
                    rpc.close()
                for server in servers:
                    server.shutdown()
                    server.server_close()
                for thread in threads:
                    thread.join(2)

    def check_request(self, index, port, key, model):
        self.assertEqual(len(self.requests), index+1, self.requests)
        request = self.requests[index]
        self.assertEqual((request['port'], request['authorization'], request['body']['model']),
                         (port, 'Bearer '+key, model))

    def test_provider_only_endpoint_and_key_reload(self):
        with self.running() as (path, rpc):
            path.write_text(config(port=28181, key='SYNTHETIC_B_KEY'))
            rpc.request('_x.ai/internal/reload_models', {})
            rpc.prompt_new()
            self.check_request(1, 28181, 'synthetic-b-key', 'synthetic-model-a')

    def test_new_provider_and_model_reload(self):
        with self.running() as (path, rpc):
            path.write_text(config()+config(port=28181, key='SYNTHETIC_B_KEY',
                                            provider='provider-b', model='model-b'))
            rpc.request('_x.ai/internal/reload_models', {})
            rpc.prompt_new('model-b')
            self.check_request(1, 28181, 'synthetic-b-key', 'synthetic-model-b')

    def test_invalid_reload_retains_last_valid_routing(self):
        with self.running() as (path, rpc):
            path.write_text(config().replace('base_url =', 'base_ur ='))
            result = rpc.request('_x.ai/internal/reload_models', {}, allow_error=True)
            self.assertIn('error', result)
            self.assertNotIn('synthetic-a-key', json.dumps(result))
            rpc.prompt_new()
            self.check_request(1, 28081, 'synthetic-a-key', 'synthetic-model-a')

    def test_new_provider_inline_auth_is_reloaded_with_provider(self):
        with self.running() as (path, rpc):
            second = config(port=28181, key='SYNTHETIC_B_KEY', provider='provider-b', model='model-b')
            second = second.replace('env_key = "SYNTHETIC_B_KEY"\n', '')
            second += '[model_providers.provider-b.auth]\ncommand = "printf synthetic-b-key"\n'
            path.write_text(config()+second)
            rpc.request('_x.ai/internal/reload_models', {})
            rpc.prompt_new('model-b')
            self.check_request(1, 28181, 'synthetic-b-key', 'synthetic-model-b')

    def test_named_auth_helper_only_change_is_reloaded(self):
        initial = config().replace('env_key = "SYNTHETIC_A_KEY"', 'auth_provider = "synthetic"')
        initial += '[auth_provider.synthetic]\ncommand = "printf synthetic-a-key"\n'
        with self.running(initial=initial) as (path, rpc):
            path.write_text(initial.replace('printf synthetic-a-key', 'printf synthetic-b-key'))
            rpc.request('_x.ai/internal/reload_models', {})
            rpc.prompt_new()
            self.check_request(1, 28081, 'synthetic-b-key', 'synthetic-model-a')

    def test_builtin_override_errors_are_distinct_and_make_no_request(self):
        cases = [('api_key', 'missing endpoint'), ('future_display_hint', 'unknown field')]
        for field, reason in cases:
            with self.subTest(field=field), tempfile.TemporaryDirectory(prefix='astra-builtin-error-') as folder:
                root = Path(folder)
                for name in ('home', 'grok', 'work', 'tmp'):
                    (root/name).mkdir()
                (root/'grok/config.toml').write_text(f'[model.grok-build]\n{field} = "synthetic-do-not-echo"\n')
                servers, threads = [], []
                try:
                    for port in (28081, 28182):
                        server = protocol.Server(('127.0.0.1', port), protocol.ProbeHandler)
                        server.requests = []
                        servers.append(server)
                        thread = threading.Thread(target=server.serve_forever, daemon=True)
                        thread.start()
                        threads.append(thread)
                    run = subprocess.run([str(self.binary), '-p', 'Synthetic builtin override.',
                        '--model', 'grok-build', '--output-format', 'json', '--max-turns', '1',
                        '--disable-web-search'], cwd=root/'work',
                        env=protocol.isolated_env(root, 'http://127.0.0.1:28182/v1'),
                        text=True, capture_output=True, timeout=30)
                    output = run.stdout+run.stderr
                    self.assertNotEqual(run.returncode, 0, output)
                    self.assertTrue(all(not server.requests for server in servers))
                    self.assertNotIn('synthetic-do-not-echo', output)
                    self.assertIn('grok-build', output)
                    self.assertIn(reason, output)
                finally:
                    for server in servers:
                        server.shutdown()
                        server.server_close()
                    for thread in threads:
                        thread.join(2)


if __name__ == '__main__':
    unittest.main()
