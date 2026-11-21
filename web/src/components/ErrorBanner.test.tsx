import { describe, expect, it } from 'vitest';
import { ErrorBanner } from './ErrorBanner';
import { interact, mount } from '../test/render';

describe('ErrorBanner', () => {
  it('shows a short error whole and offers nothing to expand', () => {
    const view = mount(<ErrorBanner detail="unauthorized: bad token" />);
    const banner = view.container.querySelector('.banner-error');
    expect(banner?.getAttribute('role')).toBe('alert');
    expect(banner?.textContent).toContain('unauthorized: bad token');
    expect(view.container.querySelector('.link-button')).toBeNull();
    view.unmount();
  });

  it('keeps a stack to two lines and expands the rest on request', () => {
    const detail = ['read failed', '  at open (fs.js:12)', '  at load (app.js:3)', '  at main (main.js:9)'].join('\n');
    const view = mount(<ErrorBanner title="读取这个会话失败：" detail={detail} />);
    const text = () => view.container.querySelector('.banner-detail')?.textContent ?? '';
    // The title and the first line read as one sentence; the frame on line two is
    // the one that identifies the failure, so it is on screen too.
    expect(text()).toBe('读取这个会话失败：read failed\n  at open (fs.js:12)');
    expect(text()).not.toContain('main.js:9');

    const toggle = view.container.querySelector('.link-button');
    expect(toggle?.textContent).toBe('展开其余 2 行');
    expect(toggle?.getAttribute('aria-expanded')).toBe('false');
    interact(() => {
      (toggle as HTMLButtonElement).click();
    });
    expect(text()).toContain('main.js:9');
    expect(view.container.querySelector('.link-button')?.textContent).toBe('收起');
    view.unmount();
  });

  it('runs the action the banner was given', () => {
    let reloads = 0;
    const view = mount(
      <ErrorBanner
        title="读取这个会话失败："
        detail="boom"
        action={{
          label: '重试',
          onClick: () => {
            reloads += 1;
          },
        }}
      />,
    );
    const buttons = view.container.querySelectorAll('.link-button');
    expect(buttons).toHaveLength(1);
    interact(() => {
      (buttons[0] as HTMLButtonElement).click();
    });
    expect(reloads).toBe(1);
    view.unmount();
  });

  it('does not count a trailing newline as another line', () => {
    const view = mount(<ErrorBanner detail={'one\ntwo\n'} />);
    expect(view.container.querySelector('.link-button')).toBeNull();
    expect(view.container.querySelector('.banner-detail')?.textContent).toBe('one\ntwo');
    view.unmount();
  });
});
