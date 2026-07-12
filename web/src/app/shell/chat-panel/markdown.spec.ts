import { renderChatMarkdown } from './markdown';

describe('renderChatMarkdown', () => {
  it('renders bold text', () => {
    expect(renderChatMarkdown('**Governance:** the policies')).toContain(
      '<strong>Governance:</strong>',
    );
  });

  it('renders a bullet list', () => {
    const html = renderChatMarkdown('- one\n- two');
    expect(html).toContain('<li>one</li>');
    expect(html).toContain('<li>two</li>');
  });

  it('opens links in a new tab without leaking window.opener', () => {
    const html = renderChatMarkdown('[Vanta](https://vanta.com)');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).toContain('href="https://vanta.com"');
  });

  it('separates paragraphs on a blank line, not a single newline', () => {
    const html = renderChatMarkdown('first line\nsecond line');
    expect(html).not.toContain('</p><p>');
  });

  it('keeps a single typed newline as a line break within the paragraph', () => {
    const html = renderChatMarkdown('first line\nsecond line');
    expect(html).toContain('<br>');
  });
});
