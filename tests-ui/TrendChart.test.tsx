import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
vi.mock('echarts/core', () => ({ use: vi.fn(), init: () => ({ setOption: vi.fn(), resize: vi.fn(), dispose: vi.fn() }) }));
import TrendChart from '../src/TrendChart';
import { row } from './fixtures';

describe('full-column trend interaction', () => {
  it('hits missing and zero columns, updates stationary tooltip, and clears on leaving the plot', () => {
    const rows = [row(), row({ key: '1', hour: 1, label: '01:00–02:00', coveredSeconds: 60 }),
      row({ key: '2', hour: 2, label: '02:00–03:00', future: true })];
    const { rerender } = render(<TrendChart rows={rows} daily={false} onSelect={vi.fn()} />);
    fireEvent.pointerEnter(screen.getByTestId('column-0'));
    expect(screen.getByRole('tooltip')).toHaveTextContent('该时段未采集');
    fireEvent.pointerEnter(screen.getByTestId('column-1'));
    expect(screen.getByRole('tooltip')).toHaveTextContent('已采集，代理流量 0 B');
    rerender(<TrendChart rows={[rows[0], { ...rows[1], downloaded: '2048' }, rows[2]]} daily={false} onSelect={vi.fn()} />);
    expect(screen.getByRole('tooltip')).toHaveTextContent('2.00 KiB');
    fireEvent.pointerEnter(screen.getByTestId('column-2'));
    expect(screen.getByRole('tooltip')).toHaveTextContent('该时段尚未开始');
    expect(Number.parseFloat(screen.getByRole('tooltip').style.left)).toBeGreaterThanOrEqual(0);
    fireEvent.pointerLeave(screen.getByTestId('plot-columns'));
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
  });
  it('preserves hourly arrow selection, daily Enter drilldown and click on empty columns', () => {
    const select = vi.fn();
    const rows = [row(), row({ key: '1', hour: 1 })];
    const { rerender } = render(<TrendChart rows={rows} selectedKey="0" daily={false} onSelect={select} />);
    fireEvent.keyDown(screen.getByRole('group'), { key: 'ArrowRight' });
    expect(select).toHaveBeenLastCalledWith(rows[1]);
    fireEvent.click(screen.getByTestId('column-0'));
    expect(select).toHaveBeenLastCalledWith(rows[0]);
    select.mockClear();
    rerender(<TrendChart key="daily" rows={rows} daily onSelect={select} />);
    fireEvent.keyDown(screen.getByRole('group'), { key: 'End' });
    expect(select).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByRole('group'), { key: 'Enter' });
    expect(select).toHaveBeenCalledWith(rows[1]);
  });
  it('follows an externally selected hour after the chart previously focused another column', () => {
    const rows = Array.from({ length: 24 }, (_, hour) => row({ key: String(hour), hour }));
    const select = vi.fn();
    const { rerender } = render(<TrendChart rows={rows} selectedKey="9" daily={false} onSelect={select} />);
    fireEvent.click(screen.getByTestId('column-9'));
    rerender(<TrendChart rows={rows} selectedKey="13" daily={false} onSelect={select} />);
    expect(screen.getByTestId('column-9')).not.toHaveClass('is-focused');
    expect(screen.getByTestId('column-13')).toHaveClass('is-focused', 'is-selected');
    fireEvent.keyDown(screen.getByRole('group'), { key: 'ArrowRight' });
    expect(select).toHaveBeenLastCalledWith(rows[14]);
  });
});
