package me.hydos.rosella.scene.object;

import me.hydos.rosella.Rosella;
import me.hydos.rosella.device.VulkanDevice;
import me.hydos.rosella.memory.Memory;
import me.hydos.rosella.render.info.InstanceInfo;
import me.hydos.rosella.render.info.RenderInfo;
import me.hydos.rosella.render.material.Material;
import me.hydos.rosella.render.resource.Resource;
import me.hydos.rosella.render.shader.ubo.RenderObjectUbo;
import org.joml.Matrix4f;

import java.util.concurrent.ExecutionException;
import java.util.concurrent.Future;

public class RenderObject implements Renderable {

    private final Material material;
    private final Resource modelId;
    public final Future<RenderInfo> renderInfo = null;//new RenderInfo(new BufferVertexConsumer(VertexFormats.POSITION_COLOR3_UV0));
    public InstanceInfo instanceInfo;

    public final Matrix4f modelMatrix = new Matrix4f();
    public final Matrix4f viewMatrix;
    public final Matrix4f projectionMatrix;

    public RenderObject(Resource model, Material material, Matrix4f projectionMatrix, Matrix4f viewMatrix) {
        this.material = material;
        this.modelId = model;
        this.projectionMatrix = projectionMatrix;
        this.viewMatrix = viewMatrix;
        loadModelInfo();
    }

    public void loadModelInfo() {
        // FIXME redo after fixing BufferVertexConsumer
//        ModelLoader.SimpleModel model = ModelLoader.loadModel(modelId, Assimp.aiProcess_FlipUVs | Assimp.aiProcess_DropNormals);
//        int vertexCount = model.getPositions().size();
//
//        BufferVertexConsumer vertexConsumer = (BufferVertexConsumer) renderInfo.bufferProvider;
//
//        vertexConsumer.clear();
//        Vector3f color = new Vector3f(1.0f, 1.0f, 1.0f);
//        for (int i = 0; i < vertexCount; i++) {
//            Vector3fc pos = model.getPositions().get(i);
//            Vector2fc uvs = model.getTexCoords().get(i);
//            // TODO: is this conversion doing what it should be? should convert int representing unsigned byte to signed byte through wrapping
//            vertexConsumer
//                    .pos(pos.x(), pos.y(), pos.z())
//                    .color((byte) (int) color.x(), (byte) (int) color.y(), (byte) (int) color.z())
//                    .uv(uvs.x(), uvs.y())
//                    .nextVertex();
//        }
//
//        renderInfo.indices = new IntArrayList(model.getIndices().size());
//        renderInfo.indices.addAll(model.getIndices());
    }

    @Override
    public void onAddedToScene(Rosella rosella) {
        // FIXME
//        instanceInfo = new InstanceInfo(new RenderObjectUbo(rosella.common.device, rosella.common.memory, this, material.getShaderProgram()), material);
//        this.projectionMatrix = rosella.getCamera().getProj();
//        this.viewMatrix = rosella.getCamera().getView();
    }

    @Override
    public void free(VulkanDevice device, Memory memory) {
        instanceInfo.free(device, memory);
        try {
            renderInfo.get().free(device, memory);
        } catch (InterruptedException | ExecutionException e) {
            Rosella.LOGGER.error("Error freeing render info", e);
        }
    }

    @Override
    public void rebuild(Rosella rosella) {
        instanceInfo.rebuild(rosella);
    }

    @Override
    public void hardRebuild(Rosella rosella) {
        instanceInfo.hardRebuild(rosella);
    }

    @Override
    public InstanceInfo getInstanceInfo() {
        return instanceInfo;
    }

    @Override
    public Future<RenderInfo> getRenderInfo() {
        return renderInfo;
    }
}
