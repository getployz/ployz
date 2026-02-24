package node

import (
	"encoding/json"
	"fmt"
	"math"
	"os"
	"sort"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/types"

	"github.com/spf13/cobra"
)

func lagCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags
	var jsonOutput bool

	cmd := &cobra.Command{
		Use:   "lag",
		Short: "Show replication lag and ping latency across nodes",
		RunE: func(cmd *cobra.Command, args []string) error {
			clusterName, svc, _, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			// Fan out GetPeerHealth to all nodes via the proxy.
			ctx := client.ProxyMachinesContext(cmd.Context(), nil)
			responses, err := svc.GetPeerHealth(ctx)
			if err != nil {
				return err
			}

			if jsonOutput {
				enc := json.NewEncoder(os.Stdout)
				enc.SetIndent("", "  ")
				return enc.Encode(responses)
			}

			return printLagMatrix(clusterName, responses)
		},
	}

	cf.Bind(cmd)
	cmd.Flags().BoolVar(&jsonOutput, "json", false, "Output raw JSON")
	return cmd
}

// matrixCell holds a single value in a display matrix.
type matrixCell struct {
	value float64
	warn  bool
	set   bool
}

// matrixData holds pre-built matrix data for rendering.
type matrixData struct {
	title string
	cells [][]matrixCell
	// formatCell renders a cell value to a display string.
	formatCell func(c matrixCell) string
}

func printLagMatrix(clusterName string, responses []types.PeerHealthResponse) error {
	if len(responses) == 0 {
		fmt.Println(ui.Muted("no peer health data available"))
		return nil
	}

	// Collect all node IDs and build a lookup from node ID to short label.
	nodeIDs := make([]string, 0, len(responses))
	for _, r := range responses {
		if r.Error != "" {
			fmt.Fprintln(os.Stderr, ui.WarnMsg("node %s: %s", shortID(r.NodeID), r.Error))
			continue
		}
		if r.NodeID != "" {
			nodeIDs = append(nodeIDs, r.NodeID)
		}
	}
	sort.Strings(nodeIDs)

	if len(nodeIDs) == 0 {
		fmt.Println(ui.Muted("no healthy nodes reported"))
		return nil
	}

	// Index: nodeIDs[i] → index i
	idx := make(map[string]int, len(nodeIDs))
	for i, id := range nodeIDs {
		idx[id] = i
	}

	labels := make([]string, len(nodeIDs))
	for i, id := range nodeIDs {
		labels[i] = shortID(id)
	}

	n := len(nodeIDs)

	// Build replication lag matrix and ping matrix.
	lagMatrix := make([][]matrixCell, n)
	pingMatrix := make([][]matrixCell, n)
	for i := range n {
		lagMatrix[i] = make([]matrixCell, n)
		pingMatrix[i] = make([]matrixCell, n)
	}

	// Track clock health per node.
	var maxOffset float64
	allHealthy := true

	for _, r := range responses {
		if r.Error != "" || r.NodeID == "" {
			continue
		}
		obsIdx, ok := idx[r.NodeID]
		if !ok {
			continue
		}
		offset := math.Abs(r.NTP.NTPOffsetMs)
		if offset > maxOffset {
			maxOffset = offset
		}
		if !r.NTP.NTPHealthy {
			allHealthy = false
		}

		for _, p := range r.Peers {
			pIdx, ok := idx[p.NodeID]
			if !ok {
				continue
			}
			lagMatrix[obsIdx][pIdx] = matrixCell{
				value: float64(p.ReplicationLag.Milliseconds()),
				warn:  p.Stale,
				set:   true,
			}
			pingVal := float64(p.PingRTT.Microseconds()) / 1000.0
			if p.PingRTT < 0 {
				pingVal = -1
			}
			pingMatrix[obsIdx][pIdx] = matrixCell{
				value: pingVal,
				warn:  p.PingRTT < 0,
				set:   p.PingRTT != 0,
			}
		}
	}

	// Print header.
	fmt.Println(ui.InfoMsg("replication lag for cluster %s", ui.Accent(clusterName)))
	clockStatus := ui.Success("healthy")
	if !allHealthy {
		clockStatus = ui.Warn("unhealthy")
	}
	fmt.Print(ui.KeyValues("  ",
		ui.KV("nodes", fmt.Sprintf("%d", n)),
		ui.KV("clock sync", fmt.Sprintf("%s (max_offset=%.1fms)", clockStatus, maxOffset)),
	))

	// Replication lag matrix.
	renderMatrix(labels, matrixData{
		title: "replication lag (ms), row sees col",
		cells: lagMatrix,
		formatCell: func(c matrixCell) string {
			if !c.set {
				return "?"
			}
			return fmt.Sprintf("%.0f", c.value)
		},
	})

	// Ping latency matrix.
	renderMatrix(labels, matrixData{
		title: "ping latency (ms), row sees col",
		cells: pingMatrix,
		formatCell: func(c matrixCell) string {
			if !c.set {
				return "?"
			}
			if c.value < 0 {
				return "\u00d7" // ×
			}
			if c.value < 1 {
				return fmt.Sprintf("%.2f", c.value)
			}
			return fmt.Sprintf("%.1f", c.value)
		},
	})

	// Summary.
	fmt.Println()
	var worstLag float64
	var worstFrom, worstTo string
	var staleCells, unknownCells int
	for i := range n {
		for j := range n {
			if i == j {
				continue
			}
			c := lagMatrix[i][j]
			if !c.set {
				unknownCells++
				continue
			}
			if c.warn {
				staleCells++
			}
			if c.value > worstLag {
				worstLag = c.value
				worstFrom = labels[i]
				worstTo = labels[j]
			}
		}
	}
	if worstFrom != "" {
		fmt.Print(ui.KeyValues("  ",
			ui.KV("worst link", fmt.Sprintf("%s -> %s = %.0fms", worstFrom, worstTo, worstLag)),
			ui.KV("stale cells", fmt.Sprintf("%d", staleCells)),
			ui.KV("unknown cells", fmt.Sprintf("%d", unknownCells)),
		))
	}

	return nil
}

func renderMatrix(labels []string, m matrixData) {
	n := len(labels)
	fmt.Println()
	fmt.Println(ui.Muted("  " + m.title))
	fmt.Println()

	// Column width.
	colW := 10
	for _, l := range labels {
		if len(l)+2 > colW {
			colW = len(l) + 2
		}
	}

	// Header row.
	var sb strings.Builder
	sb.WriteString(fmt.Sprintf("  %-*s", colW, ""))
	for _, l := range labels {
		sb.WriteString(fmt.Sprintf("%-*s", colW, l))
	}
	fmt.Println(ui.Accent(sb.String()))

	// Data rows.
	for i := range n {
		var row strings.Builder
		row.WriteString(fmt.Sprintf("  %-*s", colW, labels[i]))
		for j := range n {
			if i == j {
				row.WriteString(ui.Muted(fmt.Sprintf("%-*s", colW, "--")))
				continue
			}
			c := m.cells[i][j]
			val := m.formatCell(c)
			padded := fmt.Sprintf("%-*s", colW, val)
			if c.warn {
				padded = ui.Warn(padded)
			} else if !c.set {
				padded = ui.Muted(padded)
			}
			row.WriteString(padded)
		}
		fmt.Println(row.String())
	}
}

func shortID(id string) string {
	if len(id) <= 8 {
		return id
	}
	return id[:8]
}
