#!/usr/bin/env python3
"""Regression checks for sample archive/model association."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('native_audit', Path(__file__).with_name('verify-native-playback.py'))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class ModelAssociation(unittest.TestCase):
    def test_parts_require_both_id_and_guid_in_the_same_archive(self):
        root = Path('/samples')
        animation = root / 'hiyori_ja/hiyori_pro/motion.can3'
        matching = animation.with_name('matching.cmo3')
        wrong = animation.with_name('wrong.cmo3')
        paths = {matching:'good', wrong:'bad'}
        models = {'good': {'parameters':[], 'sources':[{'guid':'g', 'id':'Part', 'data':{'type':'Part'}}]},
            'bad': {'parameters':[], 'sources':[{'guid':'wrong', 'id':'Part', 'data':{'type':'Part'}}]}}
        self.assertEqual(audit.compatible_models(animation, root, paths, models, set(), {('g','Part')}), [matching])

    def test_shallow_and_nested_samples_bind_with_exact_identities(self):
        root = Path('/samples')
        for folder in ['ren_ja', 'zundamon_ja', 'hiyori_ja/hiyori_free', 'hiyori_ja/hiyori_pro']:
            animation = root / folder / 'motion.can3'
            model = root / folder / 'model.cmo3'
            wrong_archive = root / 'other' / 'model.cmo3'
            mismatched = root / folder / 'different.cmo3'
            paths = {model: 'good', wrong_archive: 'good', mismatched: 'bad'}
            models = {'good': {'parameters': [{'guid': 'guid', 'id': 'ParamArm'}]},
                'bad': {'parameters': [{'guid': 'different', 'id': 'ParamArm'}]}}
            self.assertEqual(audit.compatible_models(animation, root, paths, models,
                {('guid', 'ParamArm')}), [model])


if __name__ == '__main__':
    unittest.main()
