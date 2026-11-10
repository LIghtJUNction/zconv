import unittest
from release import select_version


class ReleaseTests(unittest.TestCase):
    def test_bumps(self):
        for mode, expected in [('none', '1.2.3'), ('patch', '1.2.4'), ('minor', '1.3.0'), ('major', '2.0.0')]:
            self.assertEqual(select_version('1.2.3', mode), expected)

    def test_injection_and_invalid_semver(self):
        for value in ['01.2.3', '1.2', 'v1.2.3', '1.2.3\n', '1.2.3;echo bad', '$(id)', '1.2.3-rc.1']:
            with self.assertRaises(ValueError):
                select_version('1.2.3', 'none', value)

    def test_conflicting_inputs_and_downgrade(self):
        for args in [('1.2.3', 'patch', '1.2.4'), ('1.2.3', 'none', '1.2.2'), ('1.2.3', 'none', '', 'v1.2.4')]:
            with self.assertRaises(ValueError):
                select_version(*args)

    def test_tag_is_exact(self):
        self.assertEqual(select_version('0.1.0', 'none', '', 'v0.1.0'), '0.1.0')


if __name__ == '__main__':
    unittest.main()
