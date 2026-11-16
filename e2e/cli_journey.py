"""Exercise the shipped CLI across fresh processes, with isolated persistent state."""
import json
import os
import pathlib
import subprocess
import tempfile
import unittest


class TimerJourney(unittest.TestCase):
    def test_create_pause_resume_cancel_and_invalid_duration(self):
        binary = str(pathlib.Path('target/debug/bide').resolve())
        with tempfile.TemporaryDirectory() as directory:
            env = {**os.environ, 'XDG_STATE_HOME': directory, 'XDG_CONFIG_HOME': directory}

            def cli(*args, success=True):
                result = subprocess.run([binary, *args], env=env, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0 if success else 2, result.stderr)
                return result.stdout

            cli('timer', 'add', '1h', '--label', 'E2E laundry')
            entries = json.loads(cli('list', '--json'))['items']
            self.assertEqual(len(entries), 1)
            item = entries[0]
            self.assertEqual(item['label'], 'E2E laundry')
            identity = item['id']
            cli('pause', identity)
            self.assertEqual(json.loads(cli('show', identity, '--json'))['status'], 'paused')
            cli('resume', identity)
            self.assertEqual(json.loads(cli('show', identity, '--json'))['status'], 'running')
            cli('timer', 'add', 'invalid', success=False)
            self.assertEqual(len(json.loads(cli('list', '--json'))['items']), 1)
            cli('cancel', identity)
            self.assertEqual(json.loads(cli('show', identity, '--json'))['status'], 'cancelled')


if __name__ == '__main__':
    unittest.main()
