package cluster

import "ployz/internal/adapter/fake"

// Transitional re-export layer for cluster-backed fakes.
// New multi-node tests should use internal/testkit/scenario first,
// then import this package for low-level cluster controls.

type (
	Cluster                = fake.Cluster
	ClusterOption          = fake.ClusterOption
	Registry               = fake.Registry
	LinkConfig             = fake.LinkConfig
	NodeSnapshot           = fake.NodeSnapshot
	Harness                = fake.Harness
	HarnessNode            = fake.HarnessNode
	HarnessConfig          = fake.HarnessConfig
	ClusterContainerStore  = fake.ClusterContainerStore
	ClusterDeploymentStore = fake.ClusterDeploymentStore
)

var (
	ErrNodeDead               = fake.ErrNodeDead
	WithRandSeed              = fake.WithRandSeed
	NewCluster                = fake.NewCluster
	NewRegistry               = fake.NewRegistry
	NewHarness                = fake.NewHarness
	NewClusterContainerStore  = fake.NewClusterContainerStore
	NewClusterDeploymentStore = fake.NewClusterDeploymentStore
)
