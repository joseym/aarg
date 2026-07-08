import { buildPreviewModel } from './workspace.model';
import type { LineProvenance, VariantPayload } from '../../models';

/** A minimal human VariantPayload carrying two projects — one with a source
 *  url, one without — and nothing else the projects assertions care about. */
function payloadWithProjects(): VariantPayload {
  return {
    variant: 'human',
    template: 'classic',
    contact: { full_name: 'Ada', email: 'ada@example.com', phone: null, location: null, links: [] },
    target_title: 'Engineer',
    summary: '',
    roles: [],
    education: [],
    skills_section: { skills: [] },
    skill_groups: [],
    projects: [
      { id: 'project-1', name: 'AARG', summary: 'A resume tailoring CLI.', url: 'https://github.com/joseym/aarg' },
      { id: 'project-2', name: 'Sketches', summary: 'Weekend experiments.', url: null },
    ],
    achievements: [],
    certifications: [],
    layout_hints: { sidebar: false, accent_color: '', density: '', show_summary: true, max_pages: 1 },
  };
}

describe('buildPreviewModel projects', () => {
  const empty = new Map<string, LineProvenance>();

  it('builds one entry per payload project, keyed on the project id', () => {
    const m = buildPreviewModel(payloadWithProjects(), empty, {}, null);
    expect(m.projects.length).toBe(2);
    expect(m.projects[0].name).toBe('AARG');
    expect(m.projects[0].summary.key).toBe('project:project-1');
    expect(m.projects[0].summary.text).toBe('A resume tailoring CLI.');
  });

  it('carries a url for a project that has one so it renders as a link', () => {
    const m = buildPreviewModel(payloadWithProjects(), empty, {}, null);
    expect(m.projects[0].url).toBe('https://github.com/joseym/aarg');
  });

  it('leaves url null for a project without one, so no dead link renders', () => {
    const m = buildPreviewModel(payloadWithProjects(), empty, {}, null);
    expect(m.projects[1].url).toBeNull();
  });

  it('overlays a local edit on a project summary as its own edit', () => {
    const edits = { 'project:project-1': 'A tailoring CLI, reworded.' };
    const m = buildPreviewModel(payloadWithProjects(), empty, edits, null);
    expect(m.projects[0].summary.text).toBe('A tailoring CLI, reworded.');
    expect(m.projects[0].summary.status).toBe('edited');
  });

  it('has no projects when the payload carries none', () => {
    const payload = { ...payloadWithProjects(), projects: [] };
    const m = buildPreviewModel(payload, empty, {}, null);
    expect(m.projects).toEqual([]);
  });
});
