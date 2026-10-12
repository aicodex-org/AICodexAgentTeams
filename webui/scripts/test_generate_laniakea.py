"""验证流线生成器对完整和截断 FITS 主 HDU 的处理"""
import importlib.util
from pathlib import Path
import unittest

import numpy as np

spec = importlib.util.spec_from_file_location('generate_laniakea', Path(__file__).with_name('generate-laniakea.py'))
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


def fits_image(extra_cards=(), shape=(2, 2)):
    cards = ['SIMPLE  = T', 'BITPIX  = -32', f'NAXIS   = {len(shape)}']
    cards.extend(f'NAXIS{i}  = {size}' for i, size in enumerate(reversed(shape), 1))
    cards.extend(extra_cards)
    cards.append('END')
    header = ''.join(card.ljust(80) for card in cards).ljust(2880).encode('ascii')
    return header + np.arange(np.prod(shape), dtype='>f4').tobytes()


class FitsReaderTests(unittest.TestCase):
    def test_reads_axis_order_and_big_endian_values(self):
        values = generator.read_fits(fits_image(shape=(2, 3)))
        np.testing.assert_array_equal(values, [[0, 1, 2], [3, 4, 5]])

    def test_rejects_missing_end(self):
        with self.assertRaisesRegex(ValueError, 'missing END'):
            generator.read_fits(b'SIMPLE  = T'.ljust(2880))

    def test_rejects_incomplete_header_card(self):
        with self.assertRaisesRegex(ValueError, 'missing END'):
            generator.read_fits(b'END')

    def test_rejects_truncated_data(self):
        with self.assertRaisesRegex(ValueError, 'truncated'):
            generator.read_fits(fits_image()[:-1])

    def test_rejects_scaled_data(self):
        with self.assertRaisesRegex(ValueError, 'unscaled'):
            generator.read_fits(fits_image(extra_cards=['BSCALE  = 2']))

    def test_rejects_empty_dimensions(self):
        with self.assertRaisesRegex(ValueError, 'positive'):
            generator.read_fits(fits_image(shape=(0,)))


if __name__ == '__main__':
    unittest.main()
