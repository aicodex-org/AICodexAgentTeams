"""从 CosmicFlows 官方 FITS 网格生成主页流线，需要 NumPy

用法: python3 webui/scripts/generate-laniakea.py velocity.fits basins.zip
数据源与积分参数写入产物，浏览器不下载原始科学数据
"""
import hashlib
import json
import math
from pathlib import Path
import sys
import zipfile

import numpy as np

BASE = 'https://projets.ip2i.in2p3.fr/cosmicflows/'


def read_fits(raw):
    # 只读取无缩放的主 HDU，截断文件必须及时报错
    header = {}
    for offset in range(0, len(raw) - 79, 80):
        card = raw[offset:offset + 80].decode('ascii')
        key = card[:8].strip()
        if key == 'END':
            offset = ((offset + 80 + 2879) // 2880) * 2880
            break
        if card[8:10] == '= ':
            header[key] = card[10:].split('/')[0].strip()
    else:
        raise ValueError('FITS header is missing END')
    if header.get('SIMPLE') != 'T' or 'BSCALE' in header or 'BZERO' in header:
        raise ValueError('Expected an unscaled primary FITS HDU')
    dtype = np.dtype({-32: '>f4', -64: '>f8', 32: '>i4', 64: '>i8'}[int(header['BITPIX'])])
    shape = tuple(int(header[f'NAXIS{i}']) for i in range(int(header['NAXIS']), 0, -1))
    if not shape or any(size <= 0 for size in shape):
        raise ValueError('FITS dimensions must be positive')
    count = math.prod(shape)
    if offset + count * dtype.itemsize > len(raw):
        raise ValueError('FITS data is truncated')
    return np.frombuffer(raw, dtype=dtype, count=count, offset=offset).reshape(shape)


def main():
    velocity_raw = Path(sys.argv[1]).read_bytes()
    basin_path = Path(sys.argv[2])
    if basin_path.suffix == '.zip':
        with zipfile.ZipFile(basin_path) as archive:
            basin_raw = archive.read('CF4_new_128-z008_BoA.fits')
    else:
        basin_raw = basin_path.read_bytes()
    # 速度 FITS 的轴顺序反映到 NumPy 后为 XYZ，分量也为 XYZ
    field = read_fits(velocity_raw).astype(float) * 52
    basin = read_fits(basin_raw)
    if field.shape != (3, 64, 64, 64) or basin.shape != (128, 128, 128):
        raise ValueError('Expected the CF4 64-cubed velocity and 128-cubed basin grids')
    if not np.isfinite(field).all():
        raise ValueError('Velocity grid contains non-finite values')
    # 盆地文件的 NumPy 轴顺序为 ZYX，与速度文件不同
    # 以论文表 A.1 的吸引子 [-62, -8, 39] 属于标签 1 交叉校验
    seeds = np.argwhere(basin == 1)[:, ::-1]
    if not len(seeds):
        raise ValueError('Basin grid has no Laniakea label 1 voxels')
    rng = np.random.default_rng(2014)
    seeds = seeds[rng.choice(len(seeds), min(1200, len(seeds)), replace=False)]
    positions = (seeds.astype(float) - 64) * (1000 / 128)

    def velocity(points):
        grid = points / (1000 / 64) + 32
        low = np.floor(grid).astype(int)
        low = np.clip(low, 0, 62)
        frac = np.clip(grid - low, 0, 1)
        result = np.zeros_like(points)
        for z in (0, 1):
            for y in (0, 1):
                for x in (0, 1):
                    delta = np.array([x, y, z])
                    index = low + delta
                    weight = np.prod(np.where(delta, frac, 1 - frac), axis=1)
                    result += field[:, index[:, 0], index[:, 1], index[:, 2]].T * weight[:, None]
        return result

    def direction(points):
        result = velocity(points)
        speed = np.linalg.norm(result, axis=1)
        return result / np.maximum(speed[:, None], 1e-9), speed

    # 论文坐标来自 128³ 重建，仅作初值；标记必须与绘图使用的 64³ 插值速度场一致
    # https://arxiv.org/html/2305.02339v2#S3
    published_attractor = np.array([-62., -8., 39.])
    attractor = published_attractor.copy()
    offsets = np.eye(3) * .01
    for _ in range(30):
        v = velocity(attractor[None, :])[0]
        jacobian = ((velocity(attractor + offsets) - velocity(attractor - offsets)) / .02).T
        if np.linalg.norm(v) < 1e-6:
            break
        attractor -= np.linalg.solve(jacobian, v)
        if not np.isfinite(attractor).all() or np.linalg.norm(attractor - published_attractor) > 2 * (1000 / 64):
            raise ValueError('Attractor solver left the Laniakea reference neighborhood')
    residual = float(np.linalg.norm(velocity(attractor[None, :])[0]))
    if residual >= 1e-6 or np.any(np.linalg.eigvals(jacobian).real >= 0):
        raise ValueError('Expected a converged, stable velocity-field attractor')
    basin_index = np.rint(attractor / (1000 / 128) + 64).astype(int)
    if basin[tuple(basin_index[::-1])] != 1:
        raise ValueError('Computed attractor is outside the Laniakea basin')

    paths = [[p.copy()] for p in positions]
    active = np.ones(len(positions), dtype=bool)
    step = 1.5
    for _ in range(200):
        k1, speed = direction(positions)
        k2, _ = direction(positions + step * k1 / 2)
        k3, _ = direction(positions + step * k2 / 2)
        k4, _ = direction(positions + step * k3)
        next_positions = positions + step * (k1 + 2*k2 + 2*k3 + k4) / 6
        active &= (speed > 8) & (np.linalg.norm(next_positions - positions, axis=1) > .15)
        active &= np.all(np.abs(next_positions) < 490, axis=1)
        for i in np.flatnonzero(active):
            # 到达吸引子附近或进入数值回环时结束，避免伪造闭合轨道
            if len(paths[i]) > 6 and np.linalg.norm(next_positions[i] - paths[i][-6]) < step:
                active[i] = False
            else:
                paths[i].append(next_positions[i].copy())
        positions = next_positions
        if not active.any():
            break
    lines = [np.round(np.array(path)[::2], 2).flatten().tolist() for path in paths if len(path) >= 12]
    output = {
        'metadata': {
            'dataset': 'CosmicFlows-4 ungrouped',
            'source': BASE,
            'papers': ['https://arxiv.org/abs/2211.16390', 'https://arxiv.org/abs/2305.02339'],
            'velocityFile': 'CF4_new_64-z008_velocity.fits',
            'basinFile': 'CF4_new_128-z008_BoA.fits',
            'sha256': {'velocity': hashlib.sha256(velocity_raw).hexdigest(), 'basin': hashlib.sha256(basin_raw).hexdigest()},
            'coordinates': 'SGX, SGY, SGZ in Mpc/h; index origin N/2; box 1000 Mpc/h',
            'method': 'Basin label 1 seeds; trilinear velocity interpolation; normalized RK4; display sampling, not galaxy trajectories',
            'velocityArrayAxes': 'component XYZ, X, Y, Z',
            'basinArrayAxes': 'Z, Y, X',
            'resolutionNote': '64^3 velocity field, 128^3 basin mask; illustrative integration, not a reproduction of the paper at full resolution',
            'attractor': {
                'position': attractor.tolist(),
                'publishedPosition': published_attractor.tolist(),
                'method': 'Stable zero of the same trilinearly interpolated 64^3 velocity field; Newton iteration initialized at the published 128^3 attractor',
                'residualKmS': residual,
                'source': 'https://arxiv.org/html/2305.02339v2#S3',
            },
            'seed': 2014, 'stepMpcH': step, 'maxSteps': 200, 'sampleStride': 2,
            'lineCount': len(lines),
        },
        'lines': lines,
    }
    target = Path(__file__).resolve().parents[1] / 'public/data/laniakea.json'
    metadata = json.dumps(output['metadata'], indent=2)
    # 每条流线占一行，保留可审查的差异并减少静态资源体积
    encoded_lines = ',\n'.join('    ' + json.dumps(line) for line in lines)
    target.write_text('{\n  \"metadata\": ' + metadata.replace('\n', '\n  ') + ',\n  \"lines\": [\n' + encoded_lines + '\n  ]\n}\n')
    print(f'{len(lines)} lines, {target.stat().st_size} bytes')


if __name__ == '__main__':
    main()
