export type GithubRepository = {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  default_branch: string;
  html_url: string;
  repo_updated_at: string;
};

export type GithubRepositorySelection = GithubRepository & {
  installation_id: number;
};

export type GithubBranch = {
  name: string;
};

export type GithubInstallationReposPage = {
  repositories: GithubRepository[];
  page: number;
  totalCount: number;
  hasNextPage: boolean;
};
