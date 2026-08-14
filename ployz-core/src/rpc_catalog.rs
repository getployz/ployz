#[macro_export]
macro_rules! rpc_catalog {
    ($consumer:ident) => {
        $consumer! {
            unary {
                DescribeContract: (describe_contract, "DescribeContract", DescribeContractRequest, "describe_contract"),
                Inspect: (inspect, "Inspect", InspectRequest, "inspect"),
                MachineToken: (machine_token, "MachineToken", MachineTokenRequest, "machine_token"),
                Initialize: (initialize, "Initialize", InitializeRequest, "initialize"),
                Register: (register, "Register", RegisterRequest, "register"),
                Join: (join, "Join", JoinRequest, "join"),
                ListMachines: (list_machines, "ListMachines", ListMachinesRequest, "list_machines"),
                UpdateMachine: (update_machine, "UpdateMachine", UpdateMachineRequest, "update_machine"),
                RemoveLocalMachine: (remove_local_machine, "RemoveLocalMachine", RemoveLocalMachineRequest, "remove_local_machine"),
                RemoveMachine: (remove_machine, "RemoveMachine", RemoveMachineRequest, "remove_machine"),
                InspectWireguard: (inspect_wireguard, "InspectWireguard", InspectWireGuardRequest, "inspect_wireguard"),
                ListContainers: (list_containers, "ListContainers", ListContainersRequest, "list_containers"),
                InspectContainer: (inspect_container, "InspectContainer", InspectContainerRequest, "inspect_container"),
                CreateContainer: (create_container, "CreateContainer", Box<CreateContainerRequest>, "create_container"),
                StartContainer: (start_container, "StartContainer", StartContainerRequest, "start_container"),
                StopContainer: (stop_container, "StopContainer", StopContainerRequest, "stop_container"),
                RemoveContainer: (remove_container, "RemoveContainer", RemoveContainerRequest, "remove_container"),
                CreateVolume: (create_volume, "CreateVolume", CreateVolumeRequest, "create_volume"),
                ListVolumes: (list_volumes, "ListVolumes", ListVolumesRequest, "list_volumes"),
                InspectVolume: (inspect_volume, "InspectVolume", InspectVolumeRequest, "inspect_volume"),
                RemoveVolume: (remove_volume, "RemoveVolume", RemoveVolumeRequest, "remove_volume"),
                Reset: (reset, "Reset", ResetRequest, "reset"),
            }
            server_streaming {
                ContainerLogs: (container_logs, "ContainerLogs", ContainerLogsRequest, "container_logs"),
                MachineLogs: (machine_logs, "MachineLogs", MachineLogsRequest, "machine_logs"),
            }
        }
    };
}
